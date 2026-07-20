# OdyTTY Feature Reference

Use this guide to understand OdyTTY's terminal behavior, configure the native
app, and work with tabs, panes, workspaces, remote hosts, and shell integration.
For installation and a shorter overview, start with the
[README](../README.md).

## Contents

- [Configuring OdyTTY](#configuring-odytty)
- [Terminal Compatibility](#terminal-compatibility)
- [Text, Emoji, And Graphics](#text-emoji-and-graphics)
- [Tab And Pane Workflow](#tab-and-pane-workflow)
  - [Open, Close, And Switch Tabs](#open-close-and-switch-tabs)
  - [Adjust The Tab Bar](#adjust-the-tab-bar)
  - [Split A Tab Into Panes](#split-a-tab-into-panes)
  - [Organize Workspaces And The Rail](#organize-workspaces-and-the-rail)
  - [Close Workspaces And Handle Shell Exit](#close-workspaces-and-handle-shell-exit)
  - [Restore Workspaces And Open Layouts](#restore-workspaces-and-open-layouts)
  - [Save And Reopen Named Layouts](#save-and-reopen-named-layouts)
  - [Open Local Tools](#open-local-tools)
- [Shell Integration](#shell-integration)
- [Settings And Themes](#settings-and-themes)

## Configuring OdyTTY

Most customization happens inside OdyTTY. Hand-editing a config file is
optional: the settings panel, pickers, and command palette provide the primary
in-app paths. For a shorter tour of what ships enabled and which opt-ins fit
particular workflows, start with the [settings guide](settings-guide.md).

| Task | Where to do it | What happens |
| --- | --- | --- |
| Browse and edit settings | `Ctrl+Shift+,` | Changes apply live in the terminal behind the panel |
| Find a setting | Press `/` inside Settings | Filters by name, config key, description, or group |
| Save changes | `Ctrl+S` inside Settings | Writes only changed rows to `odytty.conf` |
| Choose a theme | `Ctrl+Shift+H` or Settings → Themes | Opens the theme picker |
| Choose a font | Open a font row in Settings → Fonts | Opens the bundled and system font picker |
| Run an action | `Ctrl+Shift+P` | Opens the command palette |
| Configure tabs and panes | Settings → Layout | Groups Tabs, Workspace rail, Panel, and Panes |

The settings panel is keyboard- and pointer-driven. Arrow keys move through
sections and rows, `Enter` activates a choice, and `Esc` clears a search or
closes the panel. Clicking a numeric row starts text entry; the first keystroke
replaces the prefilled value so a new number can be typed directly.

`Ctrl+Shift+,` and Settings from the terminal content menu open the section
list. Settings from the empty tab strip, a workspace slot, or the empty
workspace rail opens Layout directly.

Edits apply live, but the config file is not changed until you press `Ctrl+S`.
Saving uses a preservation-first writeback: comments, blank lines, key order,
and unknown or future keys stay in place, while changed keys are rewritten and
missing changed keys are appended. OdyTTY saves through a same-directory
temporary file and rename instead of truncating the file in place.

`odytty.conf` and `hosts.conf` share a single-writer, temporary-file, sync, and
atomic-rename path. On Unix, a new file is created with mode `0644`; a stricter
existing mode is preserved, while group/other-write and execute bits are
clamped back to that `0644` ceiling. Windows preserves inherited ACLs.

Settings resolve in this order:

| Priority | Source | Intended use |
| --- | --- | --- |
| 1 | Built-in defaults | A complete usable setup |
| 2 | `odytty.conf` | Durable preferences |
| 3 | `ODYTTY_*` environment variables | Session-scoped overrides |

On Unix, the config path is `$XDG_CONFIG_HOME/odytty/odytty.conf`, falling back
to `~/.config/odytty/odytty.conf`. On Windows it is
`%APPDATA%\odytty\odytty.conf`. OdyTTY polls the resolved file about once per
second and applies valid external edits live; environment-pinned values remain
pinned for that session.

The optional file format is dependency-free `key = value` text with `#`
comments:

```conf
theme = odyssey-default
font_family = Victor Mono
font_size = 20.0
render_quality = high
```

See [Runtime Knobs](runtime-knobs.md) for every config key, environment
variable, range, default, and reload rule. The
[annotated config](odytty.conf.example) is a starting point for readers who
prefer to edit the file.

## Terminal Compatibility

OdyTTY owns its parser and terminal model. The supported surface covers common
shells and full-screen terminal applications:

| Area | Supported behavior |
| --- | --- |
| Text and attributes | Printing, UTF-8 chunking, SGR attributes including legacy SGR 21 double underline, 256-color, and truecolor |
| Cursor and editing | Cursor movement, erase, insert/delete character and line, insert/replace mode (IRM), repeat, and reverse index |
| Screen state | Scroll regions, origin mode, tab stops, bracketed paste, focus reporting, and alternate-screen modes 47/1047/1048/1049 |
| Character sets | G0/G1 designation, SO/SI selection, and DEC Special Graphics mapping for ncurses ACS line drawing |
| OSC sequences | OSC 0/2 titles, OSC 7 working directories, OSC 8 hyperlinks, OSC 52 clipboard write plus opt-in read, OSC 133 prompt marks, and OSC 4/10/11/12 dynamic colors |
| Queries and controls | DECRQM/DECRPM, XTWINOPS, XTGETTCAP, DECRQSS, rectangle operations, selective erase, and synchronized output mode 2026 |
| Pointer input | Broad mouse reporting, including X10, normal, button-event, any-event, focus events, UTF-8, SGR, urxvt, legacy encodings, and SGR-pixel mode 1016 |
| Keyboard input | Mode-aware legacy encoding, negotiated Kitty keyboard protocol, and IME composition |

SGR-pixel mode reports true physical pixel coordinates from the native window.
Alternate scroll mode 1007 is on by default and translates the wheel into
cursor-key presses on the alternate screen. Full-screen applications that do
not track the mouse therefore scroll at the configured `scroll_wheel_lines`
rows per notch.

The Kitty keyboard protocol is a negotiated overlay. With no Kitty flags
active, legacy bytes are preserved. Under the disambiguate flag, modified
`Enter`/`Tab`/`Backspace` (for example `Ctrl+Enter`, `Shift+Enter`,
`Ctrl+Backspace`) become distinct CSI-u sequences while the unmodified keys
stay on their legacy bytes.

xterm's modifyOtherKeys (`XTMODKEYS`, levels 1 and 2) is supported as a
compatibility layer for applications that select it by `TERM` — Vim's default
`keyprotocol` and emacs both do under `xterm-256color`, and tmux negotiates
extended keys through it. Modified keys encode as
`CSI 27 ; modifier ; codepoint ~`, the level is per-screen with the same
reset behavior as the Kitty flags, and `XTQMODKEYS` reports it. When an
application enables both protocols (fish does), non-zero Kitty flags win.

On Windows, a console application can request ConPTY Win32 input with DEC
private mode 9001. While that mode is active, OdyTTY sends complete Win32 key
records, including key-up state, virtual-key and scan codes, Unicode text,
modifier flags, and repeat counts. This preserves input such as
`Ctrl+Backspace` word deletion in PowerShell and distinct `Shift+Enter`.
The application requests the mode; there is no OdyTTY setting, and the mode is
inert on Unix. When it is inactive, Kitty, modifyOtherKeys, and legacy input
continue through their normal selection rules.

A bracketed paste is queued as one transaction containing the opening marker,
sanitized text, and closing marker, so unrelated input cannot split the frame.
Pastes larger than 32 MiB are refused. Plain, non-bracketed paste remains
deliberately chunked.

IME pre-edit appears inline at the cursor and committed text is sent to the
shell. This supports CJK input methods and compose-key or dead-key accents.

The terminal bell (`BEL`) has no audible mode:

| `bell` value | Behavior |
| --- | --- |
| `urgent` | Requests window attention when unfocused; this is the default |
| `visual` | Shows a brief, readability-safe screen flash |
| `all` | Requests attention and shows the flash |
| `off` | Disables bell feedback |

## Text, Emoji, And Graphics

### Render Text And Symbols

Victor Mono is bundled and selected by default at 20 logical pixels with line
height `1.0`. JetBrains Mono is also bundled and selectable through
`font_family`.

The font picker separates always-available **Bundled Fonts** from host
**System Fonts**. Its bundled symbol fallback is a chain of Nerd Fonts v3 and
v2 faces, so PUA prompt icons work without a host-installed Nerd font and
remain compatible with configs from either Nerd Font era.

Bundled and discovered system families both resolve without hand-written
configuration.

| Text control | Support |
| --- | --- |
| Font sources | Bundled families, system families, and direct font files |
| Styling | Font-weight variants, synthetic styles, and subpixel antialiasing |
| Programming ligatures | Default-on ASCII contextual alternates with grid-aligned source cells |
| Fallback | Per-range symbol maps and bundled Nerd Font v3/v2 faces |
| Readability | Linear-light color composition, glyph coverage gamma, stem darkening, and minimum-contrast enforcement |

Fresh profiles enable contextual programming ligatures from the selected text
font. Shaping is limited to eligible ASCII runs and changes only presentation:
the terminal model keeps one logical character per cell, so copying, selection,
search, cursor placement, and wide-cell behavior retain their ordinary
semantics. Unsupported fonts and runs render through the normal per-cell path.
Set `ligatures = off` in Settings or configuration, or
`ODYTTY_LIGATURES=off` for one launch, to restore scalar rendering; the setting
reloads live.

Decomposed combining marks stay attached to their base glyph in the monochrome
text path. Wrapped and rectangular selection copy the base followed by its marks
in stored order. If the active font lacks a combining mark, OdyTTY omits that
mark instead of drawing a tofu box over the base glyph.

Supported box-drawing, block and shade elements, Braille, Powerline separators,
and Symbols for Legacy Computing sextants and octants use OdyTTY's procedural
cell coverage instead of font outlines. The coverage meets its cell edges
exactly, keeping TUI borders, graphs, and prompt separators crisp and seamless
at every font size. `geometric_boxdraw` is on by default; `box_thickness` tunes
line weight when a different visual density is preferred.

Text colors are composed in linear light, with an sRGB surface preferred for
correct antialiased edges. `text_gamma` controls coverage weight independently,
and optional subpixel antialiasing uses dual-source blending on capable GPUs.

### Render Color Emoji

Color emoji uses `swash` and a dedicated premultiplied-RGBA atlas. Bitmap
strike color fonts are supported through Noto Color Emoji (CBDT/CBLC) on Linux
and Apple Color Emoji (sbix) on macOS. Windows currently uses the monochrome
fallback because stock Segoe UI Emoji is not discovered or bitmap-rasterized.

Variation selectors, flags, keycaps, skin tones, and common ZWJ clusters are
supported. Text-default symbols stay on the monochrome fallback path, missing
color glyphs fall back there instead of becoming tofu, and emoji pixels are not
SGR-tinted.

COLR/CPAL and SVG-in-OpenType expansion remain future work.

### Display Inline Graphics

| Protocol | Supported surface |
| --- | --- |
| Kitty graphics | Actions `t`, `T`, `p`, `d`, and `q`; raw RGB, raw RGBA, and PNG still images; direct and chunked-inline transports; opt-in file, temporary-file, and Unix POSIX shared-memory transports; image and placement ids; z-index; crop; cell scaling; and pixel offsets |
| Sixel | DEC/xterm data language, RGB/HLS color introducers, repeat, raster attributes, transparency, VT340 palette, and DECSDM |

Animation and Kitty Unicode placeholders are not supported.

<a id="native-app-workflow"></a>

## Tab And Pane Workflow

OdyTTY can run many shells in one window. Tabs hold one or more panes, while
workspaces group complete sets of tabs.

### Open, Close, And Switch Tabs

| Task | Shortcut |
| --- | --- |
| Open a new tab | `Ctrl+Shift+T` |
| Close the active tab | `Ctrl+Shift+W` |
| Switch to the next tab | `Ctrl+PageDown` |
| Switch to the previous tab | `Ctrl+PageUp` |

Closing a tab closes the whole tab, including every pane it holds. Closing a
single pane is a separate action, and closing the last tab in the last
workspace quits OdyTTY.

**Rename a tab** from its right-click menu or with **Rename Tab** in the command
palette. The custom name overrides shell title updates until an empty name
clears it, and names are session-local rather than saved across restarts.

When more than one workspace exists, a tab's right-click menu adds **Move to
Workspace…**. The picker lists the other workspaces by name and moves the
clicked tab to the selected destination.

**Duplicate Tab** in the tab menu, or `Ctrl+Shift+D`, opens a fresh shell at the
active pane's working directory. It does not copy scrollback or the running
program, and a pane without a tracked directory opens in the default one.

### Adjust The Tab Bar

The bar appears when two or more sessions exist. A single unnamed shell keeps
the full-grid view, while a renamed single tab always shows the bar so its
workflow name remains visible.

| Tab-bar control | Behavior |
| --- | --- |
| **Always show tab bar** | Keeps the bar visible with one unnamed tab; `always_show_tab_bar` is off by default |
| Drag the bottom edge | Sets `tab_bar_height` from one to five text rows |
| Double-click the bottom edge | Resets `tab_bar_height` to `auto`, the default one-row height |

The bar and a pinned workspace rail form one continuous, pixel-snapped chrome
surface with no exposed background gutter. Their shared junction has one
intentional resize seam, while the rail-to-content gap remains content padding.
The band stays opaque enough for labels to remain legible over images and
effects. Inactive tabs are dimmed; the active tab is marked by a selection-role
fill and a bright, bold label.

Labels stay centered vertically when the bar grows. Kitty and Sixel placements
use the same reserved rows as text, so inline graphics remain aligned with the
visible grid.

### Split A Tab Into Panes

| Task | Direct path |
| --- | --- |
| Split into columns, with the new pane on the right | `Ctrl+Shift+E` or **Split Right** |
| Split into rows, with the new pane below | `Ctrl+Shift+O` or **Split Down** |
| Close only the focused pane | **Close Pane**, labeled with `Ctrl+b x`, in a multi-pane content menu |
| Resize adjacent panes | Drag their divider |

The direct split chords match Ghostty's Linux defaults and work in both
single-pane and multi-pane tabs. **Close Pane** is hidden for a single-pane tab,
where closing the tab is the only close action.

Once a tab has multiple panes, a tmux-style prefix enters a transient pane
command mode. The prefix is `Ctrl+b` by default and is configurable through
`pane_prefix`.

| Key after the prefix | Action |
| --- | --- |
| `%` | Split the focused pane into columns |
| `"` | Split the focused pane into rows |
| `←` / `→` / `↑` / `↓` | Move focus to the neighboring pane |
| `o` | Cycle focus to the next pane |
| `x` | Close the focused pane |
| `z` | Zoom or unzoom the focused pane while preserving its layout |
| `Space` / `=` | Equalize split sizes |
| `Ctrl+b` | Send a literal prefix to the focused pane for a nested multiplexer |

The prefix is captured only in a multi-pane tab. A single-pane shell receives
`Ctrl+b` unchanged, and `pane_prefix=off` frees it in multi-pane tabs as well.

Each pane owns an independent PTY, terminal model, scrollback, viewport,
selection, search, and cursor. Selection and search highlights render in their
own pane, while the search query bar stays on the focused pane.

Kitty and Sixel placements are also per-pane and clipped to the pane's
sub-rectangle. Optional inactive-pane dimming uses `inactive_pane_dim`, defaults
to `0.0`, and is disabled by `render_quality=plain`; the no-dim frame remains
byte-identical.

The terminal content menu includes Settings plus **Keyboard Shortcuts**,
**Connection Manager**, **Command Palette**, **Session Replay**, **Manage
Sessions**, and **Detach & switch** in a launcher section. Items with a bound
chord show it right-aligned. A tab's own menu provides New Tab, optional New
Local Tab for a host-bound workspace, Duplicate Tab, Rename Tab, Close Tab,
Close Other Tabs, **Connect to Host…**, **Replace with Host…**, optional **Move
to Workspace…**, and New Window.

**Connect to Host…** opens a saved host in a new tab immediately after the
clicked tab without changing the clicked shell. **Replace with Host…** replaces
the clicked tab and asks for confirmation when that tab still has a program
running.

Right-clicking the empty tab strip offers New Tab, New Workspace, Open Layout,
Command Palette, and Settings.

### Organize Workspaces And The Rail

Every workspace keeps its own tabs and remembers which tab was active.
Switching workspaces swaps the complete tab strip, while a one-workspace
session adds no extra chrome.

| Task | Shortcut or control |
| --- | --- |
| Create a workspace | `Ctrl+Shift+Enter` or the rail's `+` slot |
| Open the workspace picker | `Ctrl+Shift+G` |
| Switch to the next workspace | `Ctrl+Shift+PageDown` |
| Switch to the previous workspace | `Ctrl+Shift+PageUp` |
| Duplicate a workspace | `Ctrl+Shift+Alt+D` or **Duplicate Workspace** |

The vertical workspace rail appears when a second workspace exists. Its `+`
slot rests at a visible brightness, and a dead gap row above it prevents clicks
past the last workspace from opening one accidentally.

An always-visible chevron at the rail's bottom edge toggles auto-hide and saves
the choice. The same control remains available while an auto-hidden rail is
temporarily revealed, so it can be pinned again without opening Settings.

Drag the rail's inner edge to adjust its width.

| `workspace_rail` value | Rail placement |
| --- | --- |
| `auto` | Reveals the rail after a second workspace exists; this is the default |
| `always` | Pins the rail even with one workspace |
| `left` | Pins the rail on the left |
| `right` | Pins the rail on the right |

**Drag a rail slot** to reorder workspaces. A bright rule marks the destination;
release drops the slot, while `Esc` cancels without changing the order.

A short press that stays below the drag threshold remains a normal workspace
switch. Auto-hide keeps the rail revealed throughout a drag, and the shared
pointer path behaves the same on Linux, Windows, and macOS.

The workspace menu offers **Move Up** and **Move Down** as a non-drag reorder
path with no bindable chord. It also offers New, **Duplicate**, Rename, Close
Workspace, **Bind to Host…**, **Unbind from Host**, and Settings.

Either reorder path follows the active workspace by identity, so focus does not
change, and the order persists across restart. Rename edits the label in place.

Binding a workspace routes its future tabs to the chosen saved host without
changing existing tabs. The terminal content menu exposes the same New, Rename,
Close Workspace, and Bind or Unbind actions for the active workspace.

Duplicating a workspace starts a fresh workspace whose first shell uses the
active pane's working directory. Like Duplicate Tab, it creates a new shell
rather than cloning scrollback or the running program.

The working-directory path is shared by New Tab, Duplicate Tab, and Duplicate
Workspace. It behaves consistently on Linux, macOS, and Windows, where ConPTY
honors the selected directory.

### Close Workspaces And Handle Shell Exit

Closing the last tab in a workspace closes that workspace. Closing the last
workspace quits the app.

Typing `exit` or pressing `Ctrl+D` follows `shell_exit_closes`:

| Value | Behavior |
| --- | --- |
| `workspace` | Follows the normal tab-to-workspace-to-app cascade; this is the default |
| `app` | Quits OdyTTY whenever the exit would close a workspace |

The `app` value pairs with **Restore workspaces** when the entire saved shape
should reopen on the next launch.

### Restore Workspaces And Open Layouts

Turn on `restore_workspaces` in Settings → Sessions or with
`ODYTTY_RESTORE_WORKSPACES` to reopen the previous window shape. It is off by
default.

Launching `odytty` with no arguments restores the primary instance's
workspaces, tabs, pane splits, and each pane's recorded working directory. Any
command-line argument suppresses restore.

On Unix, OdyTTY validates its final state-directory leaves as owner-private,
non-symlink directories and uses owner-private regular files for layouts,
snapshots, and diagnostics. State JSON is written through a private temporary
sibling and atomic rename. Existing direct layout JSON files are the only
entries migrated; unknown children are left untouched. A failed validation
disables the affected disk sink instead of following or repairing an unexpected
object. macOS preserves inherited ACL entries while tightening mode bits;
Windows retains its inherited-ACL behavior.

A second window leaves restore ownership with the first and shows this notice:

> Another OdyTTY window owns session restore — this window won't restore or
> autosave workspaces.

The snapshot records structure only. It never saves terminal output,
scrollback, environment, or the commands that were running. A plain local pane
therefore restores as a fresh shell at its captured directory; the live-host
and remote reconnect paths below do not re-execute captured commands.

A restored remote pane reconnects through the same `ssh` path and opens a fresh
remote login shell at the host's default directory. It does not re-enter the
recorded remote directory or restart anything that was running.

If a remote host no longer resolves, OdyTTY opens a local shell instead. If a
local directory has vanished or denies access, OdyTTY retries at the home
directory; snapshots from before remote restore support also reopen those panes
locally.

On Unix, a restored or instantiated pane reattaches when its detached session
host is still alive. A dead host silently opens a fresh shell and OdyTTY shows
a compact "N of M sessions reattached" notice.

Restore and named layouts use `%LOCALAPPDATA%` on Windows. Session-host
reattachment is Unix-only, so Windows restores always open fresh shells.

### Save And Reopen Named Layouts

A named layout can capture either the whole app or one workspace:

| Scope | Save paths |
| --- | --- |
| Every workspace, tab, split, directory, and host binding | **Save as Layout…** in the content, rail-slot, or empty-rail menu; **Save All Workspaces as Layout** in the command palette |
| One clicked or active workspace | **Save Workspace as Layout…** in a rail-slot or content menu; **Save Workspace as Layout** in the command palette |

Reusing a layout name prompts to replace the existing layout, choose another
name, or cancel.

Open **Open Layout** from the command palette, or **Open Layout…** from the
empty rail, empty tab strip, or content menu. When the current window already
contains real state, choose how to apply it:

| Choice | Result |
| --- | --- |
| **Replace** | Tears down the current workspaces and installs the saved set |
| **Add** | Appends the saved workspaces beside the current ones |
| **Cancel** | Leaves the window untouched |

A fresh window with one untouched default workspace skips this prompt and
replaces that placeholder. When no layouts exist, the picker explains how to
create one.

### Open Local Tools

| Shortcut | Action |
| --- | --- |
| `Ctrl+Shift+F` | Search scrollback |
| `Ctrl+Shift+,` | Open Settings |
| `Ctrl+Shift+H` | Open the theme picker |
| `Ctrl+Shift+B` | Open the theme builder |
| `Ctrl+Shift+P` | Open the command palette |
| `Ctrl+Shift+S` | Open the connection manager |
| `Ctrl+Shift+R` | Open session replay |
| `Ctrl+Shift+A` | Manage detached sessions on Unix; open an empty list on Windows |
| `Ctrl+Shift+C` / `Ctrl+Shift+V` | Copy or paste |
| `Shift+PageUp` / `Shift+PageDown` | Scroll the local viewport |
| `Ctrl+Shift+L` | Open keyboard quick-select hints |
| `Ctrl+Shift+Space` | Enter keyboard copy mode |
| `Ctrl+Shift+Up` / `Ctrl+Shift+Down` | Jump to the previous or next prompt mark |
| `Ctrl+Shift+K` | Clear the current shell input line (sends readline Ctrl+A, Ctrl+K; no shell integration required) |
| `Delete` / `Backspace` | Delete selected editable prompt input when shell integration allows it |

The command palette, connection manager, session replay, theme builder, and
Manage Sessions each have a discoverable menu entry and a default
`Ctrl+Shift+<letter>` shortcut. A TUI cannot receive those chords, so PTY input
is unchanged.

Launcher actions appear in the content menu, while Settings → Themes includes
an **Open Theme Builder** entry.

Prompt navigation uses `Ctrl+Shift+Up` and `Ctrl+Shift+Down`. Rebind any local
action through Settings → Input, in the **Key bindings** row, or `keybinds`:

```conf
keybinds = ctrl+alt+p=command-palette
```

## Shell Integration

### Enable Prompt-Aware Actions

OSC 133 prompt marks enable prompt jumps, deleting selected editable prompt
input, command-status gutters, and click-to-position support when the shell
advertises it.

Shell integration is on by default. Newly spawned local `bash`, `zsh`, and
`fish` shells load OdyTTY's wrapper after their normal shell config; the
wrapper only adds prompt-mark hooks and never edits your rc files. Set
`shell_integration = off` in Settings or `odytty.conf` to disable it, in which
case OdyTTY still parses marks a shell emits on its own but injects no hooks.

Existing shells do not change until restarted. Bash uses an interactive
`--rcfile`, so login-shell-only startup files remain the shell's responsibility.

| Windows shell | Integration behavior |
| --- | --- |
| `powershell` / `pwsh` | Loads an OdyTTY PowerShell profile through `-NoExit -Command`; PSReadLine drives the command-start mark |
| `cmd.exe` | Unsupported because it has no OSC 133 hook surface |

The single switch means different things per shell, so the Settings section
lists what each one delivers rather than implying a uniform capability:

| Shell | What the switch delivers |
| --- | --- |
| `bash` | Prompt marks, cwd, click-to-position, button emitters; optional prompt key enhancement |
| `zsh` | The bash set plus per-keystroke edit-region reports; optional prompt key enhancement |
| `fish` | Prompt marks, cwd, edit region, click-to-position, button emitters; fish 4+ drives the keyboard protocol itself |
| `powershell` / `pwsh` | Windows only: prompt marks, cwd, button emitters; key bindings use the PSReadLine/Console API, not a VT protocol |
| `nushell` | Configure natively: set `$env.config.shell_integration.osc133`/`osc7`/`osc2` and `use_kitty_protocol` in your nushell config; OdyTTY injects nothing |

Integration applies only to shells OdyTTY launches. Nested shells, `sudo`, and
`exec`-swaps are not covered; `fish` survives a plain nested launch through
`XDG_DATA_DIRS`, and SSH tabs keep the bash bootstrap.

All four injected shell snippets percent-encode OSC 7 working-directory paths,
including non-ASCII names. The Bash path is compatible with the Bash 3.2 that
ships on older macOS systems.

### Prompt Key Enhancement (bash/zsh)

Set `shell_key_enhancement = on` (with `shell_integration` on) to make modified
keys reachable at the `bash` and `zsh` prompt. While the prompt is active the
shell enables the Kitty keyboard protocol in disambiguate mode only, so `Ctrl+C`
still interrupts, then turns it off before each command runs so the programs the
shell launches see the terminal's default keyboard mode. Modified keys such as
`Ctrl+Enter`, `Shift+Enter`, and `Ctrl+Backspace` then arrive as distinct
escape sequences you can bind:

```sh
# bash (~/.inputrc)
"\e[13;5u": "run-this-command\n"   # Ctrl+Enter

# zsh (~/.zshrc)
bindkey '^[[13;5u' accept-line     # Ctrl+Enter
```

When the knob is on, the integration also ships default bindings so the keys do
something out of the box: `Ctrl+Backspace` deletes the previous word,
`Shift+Enter` inserts a literal newline for multi-line edits, and `Ctrl+Enter`
submits the line. Each default is skipped when you have already bound the
sequence — your `~/.bashrc`/`~/.inputrc` and `~/.zshrc` are read before the
integration, so a personal rebind always wins. To override afterwards, rebind
the sequence with `bind`/`bindkey` (for example
`bind '"\e[127;5u": kill-whole-line'`).

`fish` manages the protocol itself (use `bind ctrl-enter ...`), and PowerShell
key bindings use `Set-PSReadLineKeyHandler` through the Console API, so neither
needs this knob. On by default; set `shell_key_enhancement = off` to return the
prompt to the terminal's plain keyboard mode.

For manual setup, SSH or login shells, or explicit rc management, print and
source the integration:

```sh
eval "$(odytty shell-integration bash)"
eval "$(odytty shell-integration zsh)"
odytty shell-integration fish | source
```

Until prompt input marks are active, the content menu disables Cut and Delete
for prompt input and shows an **Enable shell integration in Settings** hint.
A plain `Delete` or `Backspace` with no selection still reaches the shell.

With a selection but no known prompt boundary, OdyTTY does not send blind edit
bytes. It clears the stale selection and shows the same hint instead of risking
a corrupted command line.

### Search Actions, History, And Directories

The command palette fuzzy-filters local actions, bounded read-only shell
history, and recent OSC 7 directories. A history or directory choice types its
text into the active pane without pressing Enter; an action runs after the
overlay closes.

Use an environment override for one session:

```sh
ODYTTY_KEYBINDS="ctrl+alt+p=command-palette" odytty
```

Environment values win for that session.

### Replay Recent Output

Session replay is the `session-replay` action, bound to `Ctrl+Shift+R`. Recording
is off by default, so enable `session_replay` before opening the scrub overlay:

```conf
session_replay = on
keybinds = ctrl+alt+r=session-replay
```

For a one-session override:

```sh
ODYTTY_SESSION_REPLAY=on ODYTTY_KEYBINDS="ctrl+alt+r=session-replay" odytty
```

The local-only ring never touches disk or the network. It is capped at 600
frames or 24 MiB, whichever limit is reached first.

The overlay is presentation-only while the live session continues underneath.
Use `←` or `→` to step, `PgUp` or `PgDn` to jump ten frames, and `Home` or `End`
to move to either end.

### Connect To Saved Or Ad-Hoc Hosts

The `connection-manager` action opens a type-to-filter host list with
`Ctrl+Shift+S`. Rebind it if desired:

```conf
keybinds = ctrl+alt+h=connection-manager
```

For a one-session override:

```sh
ODYTTY_KEYBINDS="ctrl+alt+h=connection-manager" odytty
```

Hosts come from OdyTTY's `hosts.conf`. When `ssh_config_hosts = on`, the manager
also shows name-only entries from the OpenSSH config; while it is off, OdyTTY
does not reference `~/.ssh`.

The manager is presentation-only. Selecting a host starts the system `ssh`
client in a new session.

**Connect without saving.** Type a valid `[user@]host[:port]` that matches no
saved host. The **Connect to: …** row connects with `Enter`, while
`Shift+Enter` or `Ctrl+S` connects and atomically appends a matching `Host`
block to `hosts.conf`.

Existing contents are preserved. If the alias already exists, OdyTTY connects
and reports "already saved"; embedded spaces, a leading `-`, and out-of-range
ports are rejected to prevent option injection.

**Add or edit a host.** Press `Tab` for a blank form, or the right arrow to edit
the selected OdyTTY-owned host. OpenSSH-imported rows are read-only.

| Form area | Fields and behavior |
| --- | --- |
| Connection | Alias, host, user, and port |
| Identity | `IdentityFile` path, passed to `ssh` with `-i`; it is a path, never a stored secret |
| Overrides | Integration, Reuse, and Tmux, each set to inherit, on, or off |
| Appearance | Theme, font, and title |

Saving appends a new block or edits the existing block in place while
preserving every other block, comment, and unknown field byte-for-byte.
`ssh-copy-id` remains the once-only path away from password prompts.

**Test a host.** The form runs a non-interactive background probe and reports
one of four honest results: reachable with key-based authentication,
reachable but requiring interactive authentication, host-key mismatch, or
unreachable. OdyTTY never handles a password, and normal connection still works
for an interactive-auth host.

Connection-launch and probe-start failures are shown in the pane or form rather
than leaving a selection with no visible result.

**Use the host-row menu.** Right-click a row for **Open in New Tab**, **Open in
New Workspace**, or **Bind Current Workspace**. A new host workspace is
pre-bound so its later tabs use that host too.

OdyTTY-owned rows also offer **Edit…** and confirmed **Remove…** actions.
Imported OpenSSH rows hide both actions, and dismissing the menu preserves the
manager's selection.

### Control Remote Sessions

Saved hosts support these connection behaviors:

| Setting | Default | Behavior |
| --- | --- | --- |
| `remote_integration` | `on` | Sends a temporary bash-only integration wrapper and opens an interactive shell against it |
| `remote_reuse` | `on` | Reuses a shared ControlMaster connection for later tabs to the same host |
| `remote_persist` | 10 minutes | Keeps the master socket alive after the last tab closes |
| `remote_tmux` | `off` | Wraps the remote shell in a persistent tmux session named `odytty` |

The integration wrapper writes a temporary remote rcfile and persists nothing
on the host. A non-bash shell or any bootstrap failure falls back to plain
`ssh`, and the tab title becomes `user@host`.

The tmux behavior is equivalent to:

```sh
tmux new-session -A -s odytty
```

Each saved host can override Integration, Reuse, and Tmux in `hosts.conf`.
Connection reuse is available on Unix clients; Windows clients authenticate
each connection independently through `ssh.exe`.

When a remote connection drops, the tab stays open with a reconnect prompt.
`Enter` reconnects in place, while `Esc` or `Ctrl+D` closes the tab.

### Paste Images Into Remote Sessions

Pasting a clipboard image into an integrated remote tab follows
`remote_image_paste`, which defaults to `ask`. OdyTTY first shows this
confirmation in the pane:

> upload image <size> to user@host? Enter: upload · Esc: cancel

Only `Enter` starts the transfer. The image streams over the authenticated
`ssh` connection, reusing ControlMaster when available, into an unguessable
`0600` file under `/tmp`.

Nothing executes remotely, and files above 10 MiB are refused. After a
successful upload, OdyTTY shows
`image uploaded <path> · copied to clipboard`, copies the remote path to the
local clipboard, and does not type it into the shell.

Paste the copied path with `Ctrl+Shift+V` where it belongs as a command
argument. Uploaded files are cleaned up best-effort when the tab closes, and
the path works for reconnected and restored remote tabs.

### Manage Detached Sessions

On Unix, detached sessions can be managed inside the window as well as from the
CLI.
The `session-attach` action, `Ctrl+Shift+A`, and **Manage Sessions** all open the
live detached-session list. Choosing an already-open session switches to its
tab; another session prompts for **New tab** or **Replace**.

Right-click a session to kill it after confirmation. **Detach & switch** gives
the focused pane's working directory to a fresh managed session.

Attaching reconnects the live PTY and terminal model. The session host keeps
both alive through detach and attach cycles until the child exits or the idle
timeout reaps it.

Snapshot format v3 preserves G0/G1 character-set designation and SO/SI
selection, so an ACS box-drawing run survives reattach. Older v1 and v2
snapshots remain readable and restore the power-on ASCII character-set state.

On Windows, Manage Sessions opens an empty list and attach is unavailable.

### Open Interactive Paths

Interactive paths are off by default through `interactive_paths`. When enabled,
Ctrl+click on Linux/Windows or Cmd+click on macOS opens a detected path: text
files open in the configured editor with `line:col` positioning, while png, jpg,
jpeg, and webp files open in an in-app lightbox.

Dismiss the lightbox with `Esc` or a click outside. A click hint and the path
menu expose Open, **Open With…**, Copy Path, Copy File, and Reveal in File
Manager.

Path detection currently recognizes POSIX path shapes (`/`, `~/`, `./`, `../`).
Windows drive-absolute (`C:\`, `C:/`), UNC (`\\server\share`), and
backslash-relative paths are also recognized. Drive-relative forms such as
`C:folder` are deliberately not detected because their meaning depends on
per-drive process state.

Opening uses `xdg-open` on Linux, `open` on macOS, or `explorer` on Windows,
each with a scheme allowlist. OdyTTY passes the target as a single argument and
never interpolates a path into a shell command line.

### Rebind Local Actions

The `keybinds` setting and `ODYTTY_KEYBINDS` override local actions:

| Scope | Actions |
| --- | --- |
| Global | `search`, `settings`, `theme-picker`, `theme-builder`, `copy`, `paste`, `scroll-up`, `scroll-down`, `jump-prompt-prev`, `jump-prompt-next`, `copy-mode`, `hints`, `clear-input`, `command-palette`, `session-replay`, `connection-manager`, `session-attach`, `new-tab`, `new-window`, `next-tab`, `prev-tab`, `close-tab`, and `duplicate-tab` |
| Workspace | `new-workspace`, `duplicate-workspace`, `close-workspace`, `rename-workspace`, `next-workspace`, `prev-workspace`, and `workspace-picker` |
| Pane | `split-columns`, `split-rows`, `focus-pane-left`, `focus-pane-right`, `focus-pane-up`, `focus-pane-down`, `focus-pane-next`, `close-pane`, `zoom-pane`, and `equalize-panes` |

For pane actions, the binding is the key pressed after the prefix:

```conf
keybinds = ctrl+f=zoom-pane
```

See [Keybindings](keybindings.md) for every default chord, the pane prefix,
copy mode, hints, overlay navigation, and rebinding.

## Settings And Themes

Use [Configuring OdyTTY](#configuring-odytty) for the in-app settings workflow.
This section covers theme selection, background images, and transparency.

### Cursor Presentation

Fresh profiles use a blinking Block cursor with cursor slide, trail, blink
fade, and the restrained shape-aware glow enabled. Nearby eligible moves glide
while the logical cursor and input remain immediate. Stable jumps beyond the
six-cell glide range use the selected `cursor_trail_strength` presentation
profile (`balanced` by default): one cursor-shaped follower stretches into the
destination without delaying terminal state or input.

Keyboard and IME activity keep a requested blinking cursor visible; blinking
resumes after a short quiet period and parks visibly on after prolonged idle.
In an unfocused window, a Block cursor becomes a hollow outline while Bar and
Underline keep their normal forms. Cursor motion, trail, glow, easing, and
follower presentation advance only in the focused pane of a split, where they
stay clipped and do not wake idle panes.

The aura follows the active Block, Bar, or Underline geometry without changing
terminal state or input behavior. Its strength is adjustable through
`cursor_glow_intensity` on a `0.0..=1.0` scale, independent of the whole-scene
bloom: `0.0` removes the aura while leaving the glow toggle on, the default is a
restrained peak, and higher values stay bounded so text remains readable. Each
presentation control remains independently configurable in Settings or
[Runtime Knobs](runtime-knobs.md); `reduced_motion =
on` makes slide, trail, glow, easing, and new-output fade static or instant
while preserving their saved choices.

### Follow The Desktop Theme

Set `theme = system` or `ODYTTY_THEME=system` to follow the desktop dark or
light preference. The default mapping uses `odyssey` for dark mode and
`odyssey-light` for light mode.

Use `follow_os_theme`, `os_theme_dark`, and `os_theme_light` to choose custom
mappings.

### Change Or Disable The Background Image

Since v0.6.0, OdyTTY ships its OdysseyOS visual identity enabled by default. The
`odyssey-default` theme is a deep forest-green palette and is also available
through the `odyssey-jungle` alias.

The original "Dark Waves" image is bundled into the binary and shown behind the
grid. It works without an external file in source builds, AppImages, and distro
packages, and carries the repository license described in
[assets/backgrounds/LICENSE](../assets/backgrounds/LICENSE).

Draw only the theme background:

```conf
background_treatment = color
```

Alternatively, keep image treatment available without selecting an image:

```conf
background_image = none
```

Choose a custom image instead:

```conf
background_treatment = image
background_image = /path/to/your/wallpaper.png
background_image_scrim = 0.5
```

PNG, JPEG, and WebP images are supported. `background_image_scrim` ranges from
`0` for no scrim to `1` for an opaque scrim, while `auto` selects a
floor-safe value.

`background_image = default`, including the unset state, selects the bundled
image again.

### Make The Window Transparent

Window transparency lets the desktop show through the terminal background.
Text, the cursor, menus, pickers, and the settings panel remain fully opaque, so
only the background fades. The selection is fully opaque by default
(`selection_opacity = 1.0`), tuned by its own strength control
(`selection_opacity`, below) independent of the window opacity.

Transparency is on by default at `window_opacity = 80`, so the desktop shows
through the background a little out of the box. Where the display server offers
no alpha compositing the window simply presents opaque. Turn it off, or set a
fully-opaque look, from Settings → Rendering or in `odytty.conf`:

```conf
window_transparency = off
# or keep it on and go fully opaque, which matches the opaque render path:
window_opacity = 100
```

`selection_opacity` tunes the text-selection highlight strength on its own axis,
from `0.0` (invisible) through `1.5` (strongest), independent of
`window_opacity`, the theme colours, and `min_contrast`. The default `1.0` is
the authored fully opaque selection. Below `1.0` is a translucent tint that
reads as a highlight rather than a solid block yet stays clearly visible over a
transparent or busy backdrop, because a selected cell's surface alpha is lifted
toward opaque as the knob rises so the selection never falls weaker than the
surrounding content. Above `1.0` the surface stays fully opaque and the
highlight colour is pushed stronger along the backdrop-to-selection vector,
bounded and in gamut. Text under the selection stays legible through the
minimum-contrast floor.

`window_opacity` is a percentage from 20 through 100 in steps of 5, with 100
fully opaque.

Two controls keep content readable as the window grows more transparent.
`colored_bg_opacity` (default `0.9`) holds a minimum background strength for
cells whose colour differs from the theme default, so colored blocks — prompt
powerline segments, button chips, and highlighted status runs — stay solid
while the plain background still lets the desktop through; `1.0` removes the
window-opacity attenuation from those cells so they keep their full configured
background opacity (literally opaque only where the cell background already is),
`0.0` disables the floor, and it has no effect at `window_opacity = 100`. `text_brightness` (default `1.0`) lifts glyph ink
toward white so text stays legible over busy backdrops, applied after the
minimum-contrast floor and leaving colour emoji unchanged. Both live in
Settings → Rendering.

Wayland supports compositing natively, X11 requires a compositor, and Windows
uses DWM. On a display server without alpha compositing, the setting has no
visible effect.

An open menu, picker, or settings panel remains an opaque surface without
making the whole window opaque. The terminal behind it continues to show the
desktop.

A configured background image is part of the background layer. With
transparency enabled, it also becomes translucent and composes over the
desktop.

Further reference:

- [Runtime Knobs](runtime-knobs.md) lists every config key, environment
  variable, range, default, and reload behavior.
- [Annotated Config](odytty.conf.example) is a complete commented example.
- [Themes](themes.md) documents the theme format and built-in roster.
- [Effects](effects.md) covers bloom, CRT, retro, background, and motion
  effects.
- [Keybindings](keybindings.md) is the complete keyboard and rebinding
  reference.
- [Accessibility](accessibility.md) covers minimum contrast, color-vision
  modes, dimming, and the bell.
