# OdyTTY Settings Guide

OdyTTY starts with its visual identity, shell-aware workflow, and readability
protections enabled. This guide describes the defaults that are most noticeable
and the opt-in features that are useful for particular workflows. The
[runtime-knob reference](runtime-knobs.md) remains the complete list of config
keys, environment variables, ranges, aliases, and reload behavior.

## How Settings Work

Open Settings with `Ctrl+Shift+,` or from a context menu. Changes apply live;
press `Ctrl+S` to save changed rows to `odytty.conf`. Clicking a numeric row
starts direct entry, and the first typed key replaces the prefilled value.

The config file lives at:

- `$XDG_CONFIG_HOME/odytty/odytty.conf` on Unix, or
  `~/.config/odytty/odytty.conf` when `XDG_CONFIG_HOME` is unset;
- `%APPDATA%\odytty\odytty.conf` on Windows.

OdyTTY checks the file about once per second and applies valid external changes
live. Values resolve from built-in defaults, then `odytty.conf`, then matching
`ODYTTY_*` environment variables. An environment override therefore pins that
setting for the process even if the file changes. Most panel rows use the same
name as their config key and show the environment variable alongside it.

## What Ships Enabled

These are the notable active defaults rather than every setting whose value is
`on`.

| Default behavior | What it does | To turn it off or down |
| --- | --- | --- |
| Transparency at 80% | Lets the desktop show through the terminal background while text and overlays remain opaque. | `window_transparency = off` or `window_opacity = 100` |
| Colored background strength at 0.9 | Keeps app-painted cells, prompt segments, and button chips from washing out as window opacity drops. | `colored_bg_opacity = 0` |
| Shell integration | Adds prompt marks, working-directory reports, prompt-aware editing, and button helpers to new supported shells without editing shell rc files. | `shell_integration = off` |
| Risky-paste confirmation | Holds original multiline or control-bearing text behind an escaped preview when the child has not enabled bracketed-paste mode. Shells and editors such as Fish normally use their own protected bracketed path. | `warn_on_risky_paste = off` is an advanced global opt-out |
| Prompt key enhancement (off by default) | In integrated Bash and Zsh prompts, gives `Ctrl+Backspace`, `Shift+Enter`, and `Ctrl+Enter` distinct word-edit, newline, and submit behavior. Existing personal bindings win. Enabling it also re-encodes every other `Ctrl+key`, so `Ctrl+C` stops interrupting and `Ctrl+D`/`Ctrl+Z` stop signalling until you bind them back — and readline, unlike ZLE, cannot bind `Ctrl+C` back at all. | `shell_key_enhancement = on` enables it; leave it off for plain prompt input |
| Click-to-position and command status gutter | Moves the prompt cursor on a supported click and marks completed commands green or red in each visible pane. Both stay inert without shell marks. | `sh_click = off` and `command_status_gutter = off` |
| Clickable buttons with iTerm2 compatibility | Lets cooperating programs render safe numeric-response buttons and accepts the native and iTerm2 spellings. | `buttons = off`, or only `buttons_iterm_compat = off` |
| New-output fade at 250 ms | Ramps only new foreground text at the live tail; cell backgrounds appear normally from the first frame. | `new_output_fade = off` |
| Clickable URLs | Opens printed allowlisted URLs on modifier-click; output never opens a URL by itself. | `interactive_urls = off` |
| Cursor motion, scroll glide, bloom, and ambient CRT | Supplies the default motion and post-process character while preserving individual opt-outs and the plain render profile. | Use the corresponding Motion or Post-process rows, or `render_quality = plain` for the direct path |

## Useful Opt-In Settings

### Clipboard Choices

`copy_on_select` ships off so completing a selection does not replace the
clipboard unexpectedly. The platform primary-selection and middle-click path
remains independent where it is available.

```conf
copy_on_select = on
```

`warn_on_risky_paste` ships on. It opens a confirmation only when the child has
bracketed-paste mode disabled and the original text contains CR/LF or a control
character other than Tab. Applications such as Fish commonly enable bracketed
paste themselves, in which case OdyTTY preserves that protected transaction
without showing a second dialog. See [Paste Safety](features.md#paste-safety)
for the complete trigger matrix, dialog outcomes, cancellation rules, and a
reliable test procedure.

```conf
warn_on_risky_paste = on
```

OSC 52 lets terminal output request clipboard access. Clipboard writes default
to `ask`, not unrestricted access. A focused request shows a consent prompt:
`Ctrl+Shift+1` allows it once, `Ctrl+Shift+S` allows writes for that PTY session,
`Ctrl+Shift+D` denies them for the session, and `Esc` cancels. Background or
unfocused sessions are denied in every mode. Use `on` only for applications
whose automatic clipboard writes are expected; `off` discards all writes.

```conf
osc52_write = on
# alternatives: ask, off
```

Clipboard reads are a separate, higher-risk surface: a terminal application
could receive whatever the local clipboard currently contains. They therefore
ship off and should be enabled only for trusted applications that require OSC
52 queries.

```conf
osc52_read = on
```

### Interactive File Paths

Interactive paths ship off so normal pointer movement never scans terminal text
or checks candidate paths against the filesystem. When enabled, modifier-hover
and modifier-click recognize real files relative to the pane directory, open
text at `line:column` in the configured editor, and show supported images in an
OdyTTY lightbox.

```conf
interactive_paths = on
```

Windows drive-absolute, UNC, and backslash-relative paths are supported.
Drive-relative forms such as `C:folder` are not detected because they depend on
per-drive process state. Set `interactive_paths_editor` to an editor name or an
argv template when `$EDITOR` or `$VISUAL` is not enough; commands are never run
through a shell.

### Restore And Replay

Workspace restore is opt-in so an ordinary launch starts predictably with a
fresh shell. It restores only the saved shape, names, split tree, and working
directories. It never restores output, scrollback, environment, or commands.

```conf
restore_workspaces = on
```

Session replay ships off so the PTY path records no frames. Enabling it keeps a
bounded local in-memory ring for the replay overlay; nothing is written to disk
or sent over the network.

```conf
session_replay = on
```

### SSH Conveniences

The connection manager uses its own `hosts.conf` by default. OpenSSH config
import is opt-in so OdyTTY does not read the local SSH config unless invited.
The bounded parser imports names and connection fields for display; it does not
surface key material or credentials.

```conf
ssh_config_hosts = on
```

Remote tmux persistence is a workflow preference and requires tmux on the
remote system. With remote integration active, it wraps the shell in
`tmux new-session -A -s odytty`; if tmux is unavailable, the connection falls
back to plain Bash.

```conf
remote_tmux = on
```

### Interface And Effects

Sticky buttons remain live in scrollback after their command ends. They ship
off because an old output control that still sends input is more surprising
than a button whose lifetime ends at the next prompt.

```conf
buttons_sticky = on
```

The tab-panel seam is a purely visual hairline between chrome and content.

```conf
tab_seam = on
```

Rail auto-hide is also a preference rather than a space-saving default. The
chevron at the bottom of the rail toggles it without opening Settings and stays
available while the rail is revealed.

```conf
workspace_rail_autohide = on
```

The ambient CRT profile is already on. The stronger retro preset ships off and
raises bloom, scanlines, and vignette values, but it does not curve the screen.
Curvature is flat by default and has no Settings row; its config key or
environment override is the only source of barrel distortion.

```conf
retro = on
crt_curvature = 0.04
```

`crt_curvature` accepts `0.0` through `0.12`. Remove it or set it to `0.0` for a
flat screen, including while the retro preset is active.

Reduced motion ships off because the default presentation includes motion.
Enable it to make cursor slide, trail, glow, blink fade, and new-output fade
static or instant without losing the stored values of their individual knobs.
The setting is explicit because OS reduced-motion discovery is not yet
available.

```conf
reduced_motion = on
```

Choose among the built-in and user themes with `Ctrl+Shift+H` or the Theme row.
The default is `odyssey-default`; `theme = system` follows the OS light/dark
direction, while an explicit theme name keeps that palette selected. See the
[theme guide](themes.md) for the library and custom-theme format.

## Tuning A Transparent Window

These controls solve different kinds of washout:

| Setting | Default and range | Reach for it when |
| --- | --- | --- |
| `window_opacity` | `80`, from `20` to `100` | The whole terminal background should show more or less of the desktop. Text and overlays do not fade with it. |
| `colored_bg_opacity` | `0.9`, from `0.0` to `1.0` | App-painted cell backgrounds are too weak. At `1.0`, window-opacity attenuation is removed and the cell keeps its full configured background opacity; it is literally opaque only when that cell background opacity is also `1.0`. |
| `text_brightness` | `1.0`, from `1.0` to `1.5` | Colored text needs a lift toward white after the minimum-contrast floor. Black ink never lifts, and color emoji are exempt. |
| `selection_opacity` | `1.0`, from `0.0` to `1.5` | The selection is too faint, too solid, or not emphatic enough. Below `1.0` thins it to a tint; above `1.0` keeps it opaque but pushes the colour stronger. Independent of window opacity. |
| `tab_panel_strength` | `0.8`, from `0.0` to `1.0` | Tab and rail labels need a quieter surface. This sets panel opacity directly; `0.0` removes the panel and `1.0` makes it nearly opaque. |

For broader readability controls, see the
[accessibility guide](accessibility.md). For every post-process relationship,
see the [effects guide](effects.md).
