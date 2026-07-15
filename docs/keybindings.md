# OdyTTY — Keyboard Reference

This is the single source of truth for OdyTTY's keyboard surface: the default
shortcuts, the tmux-style pane prefix, copy mode, keyboard hints, and how to
rebind everything. For the full config-key reference see
[`runtime-knobs.md`](runtime-knobs.md); for accessibility-oriented keys see
[`accessibility.md`](accessibility.md).

The default global chords remain Ctrl-based on macOS; Cmd+C and Cmd+V are not
default copy/paste bindings. The exception is opening links and interactive
paths, which uses Cmd+click on macOS because Ctrl+click is a secondary click.

## Contents

- [How OdyTTY's shortcuts stay out of the shell's way](#how-odyttys-shortcuts-stay-out-of-the-shells-way)
- [Global shortcuts](#global-shortcuts)
- [Shell Integration](#shell-integration)
- [Panes: the tmux-style prefix](#panes-the-tmux-style-prefix)
- [Copy mode](#copy-mode)
- [Keyboard hints (quick-select)](#keyboard-hints-quick-select)
- [Search](#search)
- [Command palette](#command-palette)
- [Overlays](#overlays)
- [Rebinding shortcuts](#rebinding-shortcuts)
- [Workspaces](#workspaces)
- [Remote reconnect prompt](#remote-reconnect-prompt)
- [See also](#see-also)

## How OdyTTY's shortcuts stay out of the shell's way

Every global OdyTTY shortcut is a `Ctrl+Shift+<key>` chord, plus
`Ctrl+Shift+Alt+D`, `Ctrl+PageUp/Down`, and `Shift+PageUp/Down`. A TUI program
cannot receive `Ctrl+Shift+<letter>`
through a PTY, so binding local actions there means the bytes a shell or
full-screen application sees are **unchanged** — OdyTTY never steals a keystroke
your program expects. Two stateful exceptions apply: the pane prefix (default
`Ctrl+b`) is captured only once a tab has more than one pane, and default-on
smart `Ctrl+C` copies and clears a live local selection instead of interrupting.
A full-screen TUI holds no local selection, so its `Ctrl+C` still interrupts.
See [Panes](#panes-the-tmux-style-prefix).

Notation: `Ctrl+Shift+,` is Control + Shift + the comma key. `Ctrl+Shift+Up` is
Control + Shift + the up arrow.

## Global shortcuts

These work anywhere in the window. All are rebindable except the direct split
chords (`Ctrl+Shift+E` / `Ctrl+Shift+O`) and the hardcoded
`Delete` / `Backspace` selection action (see
[Rebinding](#rebinding-shortcuts)).

| Chord | Action | Token |
| --- | --- | --- |
| `Ctrl+Shift+E` | Split the focused pane into columns (new pane on the right) | `split-columns` |
| `Ctrl+Shift+O` | Split the focused pane into rows (new pane below) | `split-rows` |
| `Ctrl+Shift+F` | Search the scrollback | `search` |
| `Ctrl+Shift+,` | Open the settings panel | `settings` |
| `Ctrl+Shift+H` | Open the theme picker | `theme-picker` |
| `Ctrl+Shift+B` | Open the theme builder | `theme-builder` |
| `Ctrl+Shift+P` | Open the command palette | `command-palette` |
| `Ctrl+Shift+S` | Open the connection manager | `connection-manager` |
| `Ctrl+Shift+R` | Open session replay | `session-replay` |
| `Ctrl+Shift+A` | Open Manage Sessions (Unix attach list; empty on Windows) | `session-attach` |
| `Ctrl+Shift+C` | Copy the selection | `copy` |
| `Ctrl+Shift+V` | Paste | `paste` |
| `Ctrl+Shift+Space` | Enter keyboard copy mode | `copy-mode` |
| `Ctrl+Shift+L` | Keyboard quick-select hints | `hints` |
| `Ctrl+Shift+Up` | Jump to the previous prompt mark | `jump-prompt-prev` |
| `Ctrl+Shift+Down` | Jump to the next prompt mark | `jump-prompt-next` |
| `Ctrl+Shift+K` | Clear the shell input line (sends readline Ctrl+A, Ctrl+K; no shell integration required) | `clear-input` |
| `Delete` / `Backspace` | Delete the selected editable prompt input (when shell integration allows; otherwise the key behaves normally) | — |
| `Shift+PageUp` | Scroll the viewport up one page | `scroll-up` |
| `Shift+PageDown` | Scroll the viewport down one page | `scroll-down` |
| `Ctrl+Shift+T` | New tab | `new-tab` |
| `Ctrl+Shift+D` | Duplicate the active tab (fresh shell in the active pane's directory) | `duplicate-tab` |
| `Ctrl+Shift+N` | New window (launch another OdyTTY instance) | `new-window` |
| `Ctrl+Shift+W` | Close the active tab (and all of its panes) | `close-tab` |
| `Ctrl+PageDown` | Next tab | `next-tab` |
| `Ctrl+PageUp` | Previous tab | `prev-tab` |

Notes that trip people up:

- **Copy/paste are `Ctrl+Shift+C` / `Ctrl+Shift+V`** (the Linux-terminal
  convention). By default, plain `Ctrl+C` copies and clears a live selection
  when one exists, and sends the interrupt (`^C`) otherwise; plain `Ctrl+V` is
  readline/vi verbatim-insert. Two ways to change the copy/paste feel:
  - Set `smart_ctrl_c = off` (Settings → Clipboard) to make plain `Ctrl+C`
    always interrupt.
  - To make plain `Ctrl+V` paste, bind it: `keybinds = ctrl+v=paste` (this
    shadows verbatim-insert).
- A printed `https://…` URL is clickable by default (Ctrl+click on
  Linux/Windows or Cmd+click on macOS, with the same modifier for the hover
  underline); toggle with `interactive_urls`.
- The settings panel is `Ctrl+Shift+,` (comma), **not** `Ctrl+,`.
- Prompt navigation is the `Ctrl+Shift+Up/Down` **arrows** only; there are no
  letter-key prompt-jump shortcuts.
- There are **no** `Ctrl+number` tab shortcuts; switch tabs with
  `Ctrl+PageUp/PageDown` or the command palette.
- `Ctrl+Shift+E` / `Ctrl+Shift+O` are the way to make the *first* split on a
  single-pane tab — the pane prefix below is inert until a tab already has two
  or more panes.

## Shell Integration

Prompt-aware shortcuts depend on OSC 133 marks from the shell. Set
`shell_integration = on` in Settings or `odytty.conf` to have newly spawned
local `bash`, `zsh`, and `fish` shells load OdyTTY's integration wrapper.
Windows PowerShell (`powershell` or `pwsh`) receives the same integration
snippet through its launch command. The POSIX-shell wrappers source the normal
shell config first. Each integration emits `133;A` prompt start, `133;B`
prompt-end/input-start, `133;C` command start, and `133;D` command end marks.

This unlocks prompt jumps, selected prompt-input Delete/Backspace,
click-to-position support when advertised by the shell, and command-status
features. Existing shells are not modified; restart the shell/tab after
changing the setting. Bash integration is interactive non-login via `--rcfile`,
so login-shell-only startup files remain a manual concern.

Manual setup is available with:

```sh
eval "$(odytty shell-integration bash)"
eval "$(odytty shell-integration zsh)"
odytty shell-integration fish | source
```

```powershell
Invoke-Expression (& odytty shell-integration powershell | Out-String)
```

Automatic injection is used for newly spawned PowerShell sessions when the
setting is enabled.

Until prompt input marks are active, the context-menu Cut/Delete items are
disabled with an "Enable shell integration in Settings" hint, and plain
`Delete` / `Backspace` continue to behave as normal shell keys.

Closing a tab (`Ctrl+Shift+W`) closes the **whole** tab — every pane it holds.
Closing the last tab of the last workspace quits OdyTTY. Right-click menus are
context-aware: a tab slot includes New, Duplicate, Rename, Close, Close Others,
Connect to Host, Replace with Host, optional Move to Workspace, and New Window;
the empty tab strip offers New Tab, New Workspace, Open Layout, Command Palette,
and Settings. The terminal grid opens the selection- and path-aware content
menu. Items with a bound chord show it.

## Panes: the tmux-style prefix

Once a tab has more than one pane, a tmux-style **prefix** (default `Ctrl+b`,
config key `pane_prefix`) opens a transient pane-command mode: press the prefix,
then one pane key.

| After the prefix | Action | Token |
| --- | --- | --- |
| `%` | Split the focused pane into columns (side-by-side) | `split-columns` |
| `"` | Split the focused pane into rows (stacked) | `split-rows` |
| `←` / `→` / `↑` / `↓` | Move focus to the neighbour pane | `focus-pane-left` / `-right` / `-up` / `-down` |
| `o` | Cycle focus to the next pane | `focus-pane-next` |
| `x` | Close the focused pane | `close-pane` |
| `z` | Zoom / un-zoom the focused pane (full-bleed; layout preserved) | `zoom-pane` |
| `Space` or `=` | Equalize the split sizes | `equalize-panes` |
| `Ctrl+b` (the prefix again) | Send a literal prefix byte to the focused pane (nested multiplexer) | — |

Prefix behaviour:

- The prefix is captured **only** when the active tab has more than one pane and
  no overlay/search/modal is open. On a single-pane tab the prefix byte
  (`Ctrl+b` = `0x02`) flows straight to the shell, so readline `backward-char`
  still works and the default input path is byte-identical.
- A pending prefix is forgotten after **2 seconds** if no pane key follows.
- An unrecognized second key cancels the prefix without sending anything.
- Set `pane_prefix=off` (or `none`/`disabled`) to disable the prefix entirely
  and free `Ctrl+b` in multi-pane tabs too. Any other value (e.g. `ctrl+a`) is
  parsed as a custom prefix chord.

Drag a divider between panes to resize the panes on either side. Each pane owns
an independent PTY, terminal model, scrollback, viewport, selection, search, and
cursor. Drag the top tab bar's bottom edge to make the bar taller (up to five
text rows) and the workspace rail's inner edge to set its width; double-click
either edge to snap it back to the default (`tab_bar_height` / `tab_rail_width`,
both `auto`).

## Copy mode

`Ctrl+Shift+Space` enters a vim-style keyboard mode for selecting and copying
from the scrollback without the mouse.

| Key | Action |
| --- | --- |
| `h` `j` `k` `l` / arrows | Move the cursor left / down / up / right |
| `0` | Start of line |
| `^` | First non-blank character |
| `$` | Last non-blank character |
| `w` / `b` / `e` | Word forward / back / end |
| `g` `g` | Top of scrollback |
| `G` | Bottom (live edge) |
| `Ctrl+u` / `Ctrl+d` | Half page up / down |
| `Ctrl+b` / `PageUp` | Page up |
| `Ctrl+f` / `PageDown` | Page down |
| `v` | Toggle character-wise selection |
| `V` | Toggle line-wise selection |
| `o` | Swap the selection ends |
| `y` or `Enter` | Yank (copy) the selection and exit |
| `Esc` / `q` | Clear the selection, or exit copy mode if there is none |

Rectangular/block selection and numeric motion counts are not implemented yet.

## Keyboard hints (quick-select)

`Ctrl+Shift+L` scans the visible viewport for URLs, file paths, and Git-style
hashes, labels each match with a short home-row key sequence (`asdfghjkl;`), and
copies the one you type. Type to narrow the candidates, `Backspace` to widen,
`Esc` to cancel. Matched kinds are URLs (schemes `https`, `http`, `ftps`, `ftp`,
`file`, `mailto`, `ssh`, `git`), absolute/`~`/relative paths, and 7–40 character
hex hashes. No regular-expression engine is involved; matching is deterministic.

## Search

`Ctrl+Shift+F` opens the scrollback find overlay (always case-insensitive).

| Key | Action |
| --- | --- |
| Type | Filter to matches |
| `Enter` | Jump to the next match |
| `Shift+Enter` | Jump to the previous match |
| `Backspace` | Edit the query |
| `Esc` | Close search (clears the query and match highlights) |

## Command palette

`Ctrl+Shift+P` opens a fuzzy-filtered palette over local actions, your bounded
read-only shell history, and recent `OSC 7` working directories. Up/Down move the
selection; `PageUp`/`Home` and `PageDown`/`End` jump; `Enter` runs the selection;
`Esc` closes. Selecting a history line or directory types that text into the
active pane without pressing Enter; selecting an action runs it after the overlay
closes.

The palette also exposes **Rename Tab**, which has no keyboard shortcut of its
own — the palette is the keyboard path to it (right-clicking a tab is the mouse
path).

## Overlays

Every overlay (settings, theme picker/builder, font picker, connection manager,
session replay, Manage Sessions, the image viewer, and the modal dialogs) is
presentation-only: the terminal stays live behind it and the PTY is never
blocked. They share a common navigation model — Up/Down/Left/Right to move,
`Enter` to activate, `Esc` to close — and the settings panel adds `/` to
type-to-search settings by name, config key, description, or group. Modal
dialogs print their own one-key choices
(for example the attach dialog's `[N]`ew tab / `[R]`eplace, or close-confirm's
`[Y]`/`[N]`).

In the connection manager, typing a `[user@]host[:port]` that matches no saved
host offers a **Connect to: …** row: `Enter` connects to the typed host, and
`Shift+Enter` (or `Ctrl+S`) connects and saves it to `hosts.conf`. Both keys are
shown in a hint line beneath the row.

### Add / Edit connection form

The connection manager also opens an **Add / Edit connection** form. A pinned
**+ Add connection…** row at the bottom of the list opens the Add form on `Enter`
or a click, and a key-hint line beneath it (`Tab add · → edit · Enter connect ·
Shift+Enter save typed host`) keeps those actions visible.

Opening and moving:

- `Tab` opens a blank Add form.
- `→` (right arrow) opens an Edit form pre-filled from the selected
  OdyTTY-owned host; `ssh-config`-imported rows are read-only.
- `Up`/`Down` or `Tab` move between fields; typing edits the focused field.
- `Left`/`Right` (or `Space`) cycle a three-way `inherit`/`on`/`off` override.
- `Enter` presses the focused button, `Ctrl+S` saves from anywhere, and `Esc`
  cancels.

The **IdentityFile** row:

- `Enter` (while the field is empty) or a click on the always-visible `[Browse]`
  chip (empty or filled field alike) opens a browser of candidate private keys
  under `~/.ssh` (`Up`/`Down` pick, `Enter` fills the path, `Esc` returns).
- Typing a path by hand still works.

A focused-field help line at the bottom explains each field as you move through
it. A **Test connection** button runs a background reachability and key-auth
probe and shows a tri-state result.

## Rebinding shortcuts

There are three ways to change bindings; all three take effect on the next config
poll (about once a second) with no restart.

**Config file** — set `keybinds` in `odytty.conf` to a comma- or
semicolon-separated list of `chord=action` pairs:

```conf
# odytty.conf
keybinds = ctrl+alt+p=command-palette, ctrl+alt+r=session-replay
```

**Environment variable** — `ODYTTY_KEYBINDS` uses the same syntax and wins for
that one session (handy for a one-off or a dev override):

```sh
ODYTTY_KEYBINDS="ctrl+alt+h=connection-manager" odytty
```

**In-app key-remap editor** — open the settings panel (`Ctrl+Shift+,`), pick the
keybinds row, select an action, and press the new chord. Capture is via `Enter`
after selecting a row; `Backspace` resets one row to its default; `R` resets all;
binding a chord already in use prompts to reassign or cancel. The editor covers
all bindable actions and writes the result back to `odytty.conf` byte-identically
to a hand-typed entry.

### Chord grammar

A chord is `+`-joined modifiers plus one key:

- Modifiers: `ctrl`/`control`, `shift`, `alt`/`option`,
  `super`/`meta`/`cmd`/`command`/`win`/`windows`.
- Key: a single printable character; `comma`; a named key
  (`enter`, `backspace`, `esc`, `tab`, `space`, `pageup`, `pagedown`, `home`,
  `end`, `delete`, `insert`, `up`/`down`/`left`/`right`); or `f1`–`f24`.
- `+` and `=` cannot be used as the bound key character.

### Bindable actions

These tokens are accepted on the right-hand side of `chord=action`:

- **Overlays and pickers:** `search`, `settings`, `theme-picker`,
  `theme-builder`, `command-palette`, `session-replay`, `connection-manager`,
  `session-attach`.
- **Clipboard and copy:** `copy`, `paste`, `copy-mode`, `hints`, `clear-input`.
- **Scroll and prompt:** `scroll-up`, `scroll-down`, `jump-prompt-prev`,
  `jump-prompt-next`.
- **Tabs and windows:** `new-tab`, `new-window`, `next-tab`, `prev-tab`,
  `close-tab`, `duplicate-tab`.
- **Workspaces:** `new-workspace`, `duplicate-workspace`, `close-workspace`,
  `rename-workspace`, `next-workspace`, `prev-workspace`, `workspace-picker`.
- **Panes:** `split-columns`, `split-rows`, `focus-pane-left`,
  `focus-pane-right`, `focus-pane-up`, `focus-pane-down`, `focus-pane-next`,
  `close-pane`, `zoom-pane`, `equalize-panes`.

Pane-management actions (`focus-pane-*`, `close-pane`, `zoom-pane`,
`equalize-panes`, and the prefix-table `split-*`) cannot be bound to a bare
global chord — rebinding one sets the key you press **after** the prefix, e.g.
`keybinds = ctrl+f=zoom-pane` makes the sequence `Ctrl+b` then `Ctrl+f` zoom the
pane. The direct `Ctrl+Shift+E` / `Ctrl+Shift+O` split chords are fixed
conveniences so the first split is always reachable.

## Workspaces

A **workspace** groups a set of tabs; switching workspaces swaps the entire tab
strip. Five chords are bound by default:

| Chord | Action | Token |
| --- | --- | --- |
| `Ctrl+Shift+PageDown` | Switch to the next workspace | `next-workspace` |
| `Ctrl+Shift+PageUp` | Switch to the previous workspace | `prev-workspace` |
| `Ctrl+Shift+Enter` | Create a new workspace | `new-workspace` |
| `Ctrl+Shift+Alt+D` | Duplicate the active workspace (fresh workspace in the active pane's directory) | `duplicate-workspace` |
| `Ctrl+Shift+G` | Open the workspace picker | `workspace-picker` |

Renaming and closing a workspace are unbound by default. The workspace
right-click menu covers both, while the command palette covers Rename Workspace
only. Close Workspace is also reachable from the rail close button or a chord
you assign in the settings key-remap editor or `keybinds` config:

- `rename-workspace` — unbound by default; follows the same precedent as Rename
  Tab.
- `close-workspace` — unbound by default because it is destructive.
- **Move Up** / **Move Down** (workspace right-click menu) reorder the rail;
  menu-only, no bindable chord.
- **Duplicate Workspace** (`Ctrl+Shift+Alt+D`) opens a fresh workspace whose
  first shell starts in the active pane's directory — the workspace-level mirror
  of Duplicate Tab.

**Close cascade.** Closing the last tab of a workspace closes that workspace, and
closing the last workspace quits OdyTTY. Typing `exit` or Ctrl-D is governed by
`shell_exit_closes`: the default `workspace` matches this cascade, while `app`
quits OdyTTY whenever an exit would close a workspace. The close-tab,
close-workspace, and close-pane keybinds and the rail close button always close a
single surface, in both modes.

**Palette and menu actions without a default chord.** The command palette
carries workspace and layout actions that have no default chord:

- **Rename Workspace**.
- **Bind Workspace to Host** / **Unbind Workspace From Host**, and **New Local
  Tab** (when a workspace is bound to a remote host).
- **Save All Workspaces as Layout** / **Save Workspace as Layout** /
  **Open Layout** / **Delete Layout**.

Any of the bindable workspace actions above can still be given a chord; the
layout and host-binding actions are palette- and menu-only.

## Remote reconnect prompt

When a remote SSH tab's connection drops, OdyTTY holds the tab open with an
in-pane reconnect prompt instead of closing it. While the prompt is up, keys
drive the prompt rather than the dead shell:

| Key | Action |
| --- | --- |
| `Enter` | Reconnect in the same tab (re-runs the connection; with `remote_tmux` on, reattaches the persistent session) |
| `Esc` / `Ctrl+D` | Dismiss the prompt and close the tab |

These keys are not rebindable; they are active only while a dropped remote tab
awaits reconnect.

## See also

- [`runtime-knobs.md`](runtime-knobs.md) — every config key, env var, and default.
- [`accessibility.md`](accessibility.md) — contrast floor, color-vision modes,
  dimming, motion, and bell.
- [`panes-and-sessions-design.md`](panes-and-sessions-design.md) — the design
  record behind tabs, panes, and sessions.
