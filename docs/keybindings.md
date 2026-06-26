# OdyTTY — Keyboard Reference

This is the single source of truth for OdyTTY's keyboard surface: the default
shortcuts, the tmux-style pane prefix, copy mode, keyboard hints, and how to
rebind everything. For the full config-key reference see
[`runtime-knobs.md`](runtime-knobs.md); for accessibility-oriented keys see
[`accessibility.md`](accessibility.md).

## How OdyTTY's shortcuts stay out of the shell's way

Every global OdyTTY shortcut is a `Ctrl+Shift+<key>` (or `Ctrl+PageUp/Down`,
`Shift+PageUp/Down`) chord. A TUI program cannot receive `Ctrl+Shift+<letter>`
through a PTY, so binding local actions there means the bytes a shell or
full-screen application sees are **unchanged** — OdyTTY never steals a keystroke
your program expects. The one stateful exception is the pane prefix (default
`Ctrl+b`), and it is captured **only** once a tab has more than one pane (see
[Panes](#panes-the-tmux-style-prefix)).

Notation: `Ctrl+Shift+,` is Control + Shift + the comma key. `Ctrl+Shift+Up` is
Control + Shift + the up arrow.

## Global shortcuts

These work anywhere in the window. They are all rebindable (see
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
| `Ctrl+Shift+A` | Open Manage Sessions (attach a detached session) | `session-attach` |
| `Ctrl+Shift+C` | Copy the selection | `copy` |
| `Ctrl+Shift+V` | Paste | `paste` |
| `Ctrl+Shift+Space` | Enter keyboard copy mode | `copy-mode` |
| `Ctrl+Shift+L` | Keyboard quick-select hints | `hints` |
| `Ctrl+Shift+Up` | Jump to the previous prompt mark | `jump-prompt-prev` |
| `Ctrl+Shift+Down` | Jump to the next prompt mark | `jump-prompt-next` |
| `Ctrl+Shift+K` | Clear the editable prompt input (when shell integration allows) | `clear-input` |
| `Shift+PageUp` | Scroll the viewport up one page | `scroll-up` |
| `Shift+PageDown` | Scroll the viewport down one page | `scroll-down` |
| `Ctrl+Shift+T` | New tab | `new-tab` |
| `Ctrl+Shift+W` | Close the active tab (and all of its panes) | `close-tab` |
| `Ctrl+PageDown` | Next tab | `next-tab` |
| `Ctrl+PageUp` | Previous tab | `prev-tab` |

Notes that trip people up:

- The settings panel is `Ctrl+Shift+,` (comma), **not** `Ctrl+,`.
- Prompt navigation is the `Ctrl+Shift+Up/Down` **arrows** only; there are no
  letter-key prompt-jump shortcuts.
- There are **no** `Ctrl+number` tab shortcuts; switch tabs with
  `Ctrl+PageUp/PageDown` or the command palette.
- `Ctrl+Shift+E` / `Ctrl+Shift+O` are the way to make the *first* split on a
  single-pane tab — the pane prefix below is inert until a tab already has two
  or more panes.

Closing a tab (`Ctrl+Shift+W`) closes the **whole** tab — every pane it holds.
Closing the last tab quits OdyTTY. The right-click context menu mirrors most of
these actions and labels each with its live chord.

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
cursor.

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
type-to-search settings by name. Modal dialogs print their own one-key choices
(for example the attach dialog's `[N]`ew tab / `[R]`eplace, or close-confirm's
`[Y]`/`[N]`).

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

- Modifiers: `ctrl`/`control`, `shift`, `alt`/`option`, `super`/`meta`/`cmd`/`win`.
- Key: a single printable character; `comma`; a named key
  (`enter`, `backspace`, `esc`, `tab`, `space`, `pageup`, `pagedown`, `home`,
  `end`, `delete`, `insert`, `up`/`down`/`left`/`right`); or `f1`–`f24`.
- `+` and `=` cannot be used as the bound key character.

### Bindable actions

These tokens are accepted on the right-hand side of `chord=action`:

`search`, `settings`, `theme-picker`, `theme-builder`, `copy`, `paste`,
`scroll-up`, `scroll-down`, `jump-prompt-prev`, `jump-prompt-next`, `copy-mode`,
`hints`, `clear-input`, `command-palette`, `session-replay`,
`connection-manager`, `session-attach`, `new-tab`, `next-tab`, `prev-tab`,
`close-tab`, `split-columns`, `split-rows`, `focus-pane-left`,
`focus-pane-right`, `focus-pane-up`, `focus-pane-down`, `focus-pane-next`,
`close-pane`, `zoom-pane`, `equalize-panes`.

Pane-management actions (`focus-pane-*`, `close-pane`, `zoom-pane`,
`equalize-panes`, and the prefix-table `split-*`) cannot be bound to a bare
global chord — rebinding one sets the key you press **after** the prefix, e.g.
`keybinds = ctrl+f=zoom-pane` makes the sequence `Ctrl+b` then `Ctrl+f` zoom the
pane. The direct `Ctrl+Shift+E` / `Ctrl+Shift+O` split chords are fixed
conveniences so the first split is always reachable.

## See also

- [`runtime-knobs.md`](runtime-knobs.md) — every config key, env var, and default.
- [`accessibility.md`](accessibility.md) — contrast floor, color-vision modes,
  dimming, motion, and bell.
- [`panes-and-sessions-design.md`](panes-and-sessions-design.md) — the design
  record behind tabs, panes, and sessions.
