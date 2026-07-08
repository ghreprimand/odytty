# OdyTTY Feature Reference

The complete reference for OdyTTY's terminal behavior, native app workflow,
shell integration, and configuration. The [README](../README.md) carries the
overview and install paths; this document is the deep manual for what the
terminal does and every knob that shapes it.

## Contents

- [Terminal Compatibility](#terminal-compatibility)
- [Text, Emoji, And Graphics](#text-emoji-and-graphics)
- [Native App Workflow](#native-app-workflow)
- [Shell Integration](#shell-integration)
- [Settings And Themes](#settings-and-themes)

## Terminal Compatibility

The owned parser and terminal core cover common shell and TUI behavior:
printing, UTF-8 chunking, SGR attributes including 256-color and truecolor,
cursor movement, erase, insert/delete character and line, insert/replace mode
(IRM), repeat, reverse index, scroll regions, origin mode, tab stops, bracketed
paste, focus reporting,
alternate screen modes 47/1047/1048/1049, OSC 0/2 titles, OSC 7 working
directory tracking, OSC 8 hyperlinks, OSC 52 clipboard write plus opt-in read,
OSC 133 prompt marks, OSC 4/10/11/12 dynamic colors, DECRQM/DECRPM, XTWINOPS,
XTGETTCAP, DECRQSS, rectangle operations, selective erase, synchronized output
mode 2026, and broad mouse reporting.

Mouse support includes X10/normal/button-event/any-event tracking, focus
events, UTF-8, SGR, urxvt, legacy encodings, and SGR-pixel mode 1016 with true
physical pixel coordinates from the native window. Alternate scroll mode (1007,
default on) translates the wheel into cursor-key presses on the alternate
screen, so full-screen TUIs that do not track the mouse (pagers and similar)
scroll with the wheel at the same rows-per-notch as the local viewport
(the configured `scroll_wheel_lines` amount).

Keyboard support includes mode-aware legacy encoding and the Kitty keyboard
protocol as a negotiated overlay. With no Kitty flags active, legacy bytes are
preserved. IME composition is enabled: input-method pre-edit is rendered inline
at the cursor and committed text is sent to the shell, so CJK input methods and
compose-key/dead-key accents work.

The bell (BEL) is configurable via the `bell` setting: `urgent` (default,
requests window attention when unfocused), `visual` (a brief readability-safe
screen flash), `all` (both), or `off`. There is no audible bell.

## Text, Emoji, And Graphics

Text rendering uses bundled Victor Mono by default at 20 logical pixels with
line height `1.0`. JetBrains Mono is also bundled and remains selectable via
`font_family`. The font picker groups families into **Bundled Fonts** (Victor
Mono, JetBrains Mono, always available) and **System Fonts** (host monospace
families), and either resolves with zero config. The symbol/Nerd-font fallback
is a **chain** of bundled faces (Nerd Fonts **v3** and **v2**), so PUA prompt
icons render out of the box regardless of which Nerd Font era a config emits or
whether the host has any Nerd font installed. System font families, direct font
files, font-weight variants, per-range symbol maps, synthetic styles, subpixel
AA, glyph coverage gamma, stem darkening, and minimum-contrast enforcement are
configurable.

Color emoji uses `swash` and a dedicated premultiplied-RGBA atlas. Bitmap-strike
color fonts are supported: Noto Color Emoji (CBDT/CBLC) on Linux and Apple Color
Emoji (sbix) on macOS, including variation selectors, flags, keycaps, skin
tones, and common ZWJ clusters. Text-default symbols stay on the monochrome
fallback path, and missing color glyph coverage falls back there instead of
tofu. Emoji pixels are not SGR-tinted. COLR/CPAL and SVG-in-OpenType expansion
remain future work.

Kitty graphics support includes actions `t`, `T`, `p`, `d`, and `q`; raw RGB,
raw RGBA, and PNG still images; direct, file, temp-file, and POSIX shared-memory
transports; chunking; image and placement ids; z-index; crop; cell scaling; and
pixel offsets. Sixel supports the DEC/xterm data language, RGB/HLS color
introducers, repeat, raster attributes, transparency, VT340 palette, and DECSDM.
Animation and Kitty Unicode placeholders are not supported.

## Native App Workflow

The native app runs multiple sessions. `Ctrl+Shift+T` opens a new tab,
`Ctrl+Shift+W` closes the active tab, and `Ctrl+PageDown` /
`Ctrl+PageUp` switch tabs. Closing a tab closes the **whole** tab (every pane
it holds), which is distinct from closing a single pane (see "Close Pane"
below); closing the last tab of the last workspace quits the app. The tab bar appears when
two or more sessions exist; a single shell keeps the original full-grid view.
To keep the bar visible with a single tab, turn on **Always show tab bar**
(`always_show_tab_bar`, off by default); either way, a single tab you have
renamed always shows the bar so a named "workflow" tab is never hidden. The bar
renders as a distinct band separated from the terminal body by a thin themed
line: inactive tabs are dimmed, and the active tab keeps a full-strength bold
label plus an accent underline in the theme's cursor color, all opaque so they
stay legible over background images and treatments. Inline graphics are offset
by the same reserved tab-bar row as text, so Kitty/Sixel placements stay aligned
with the visible grid while the bar is shown.

**Rename a tab** to organize your work: right-click the tab and
choose **Rename Tab**, or run **Rename Tab** from the command palette. The
custom name overrides shell title updates until you clear it (commit an empty
name to revert to the live shell title). Names are per-session and are not saved
across restarts.

**Workspaces** group sets of tabs. Each workspace keeps its own tabs and
remembers which one was active, so switching workspaces swaps the whole tab
strip at once. A session starts with a single workspace, and a single-workspace
session looks exactly as before, with no extra chrome.

Once a second workspace exists, a vertical **rail** lists them down one side.
`Ctrl+Shift+PageDown` and `Ctrl+Shift+PageUp` cycle between workspaces,
`Ctrl+Shift+Enter` creates a new workspace, and `Ctrl+Shift+G` opens the
workspace picker. The rail's `+` slot also creates a new workspace; it now rests at a more
visible brightness, and a dead gap row sits just above it, so a click just past
the last workspace is inert and never opens a workspace by accident. The rail
follows the `workspace_rail` setting: `auto` (default) reveals it once a second
workspace exists, `always` pins it even with one, and `left` / `right` pin it to
that side.

Right-clicking a workspace (or the empty rail) offers New, Rename, and Close
Workspace plus **Bind to Host…** / **Unbind from Host** for that slot. It also
offers **Move Up** / **Move Down** to reorder the slot in the rail (menu-only,
with no bindable chord); the move follows the active workspace by identity so
the focused workspace never changes, and the new order persists across restart. Rename
edits the label in place, and Bind routes the clicked workspace's new tabs to a
chosen saved host (existing tabs keep their shells). The terminal content menu
carries the same New / Rename / Close Workspace and Bind/Unbind actions, acting
on the active workspace.

Closing the last tab in a workspace closes that workspace; closing the last
workspace quits the app. Typing `exit` or Ctrl-D follows the
`shell_exit_closes` setting: the default `workspace` matches this cascade, while
`app` quits OdyTTY whenever an exit would close a workspace, pairing with
**Restore workspaces** so the set reopens next launch. When more than one workspace exists, a tab's right-click
menu adds **Move to Workspace…**, which opens a picker of the other workspaces by
name and relocates the clicked tab to the chosen one.

**Restoring a layout.** With `restore_workspaces` on (off by default; the
Sessions section of Settings, or `ODYTTY_RESTORE_WORKSPACES`), launching
`odytty` with no arguments reopens the previous window shape: its workspaces,
tabs, and pane splits, each pane at its recorded working directory. Any
command-line argument suppresses restore, and only the primary instance
restores. A second window launched while the first is running keeps the lock
owner in charge and shows a one-line notice ("Another OdyTTY window owns
session restore, this window won't restore or autosave workspaces"), so it never
fights over the saved state.

The saved snapshot records **structure only**, never terminal output,
scrollback, environment, or the commands that were running, so a restored pane
is always a fresh shell at its directory, never a replayed session. A pane that
was connected to a remote host is restored by reconnecting to that host through
the same `ssh` path, landing a fresh remote login shell at the host's own
default directory (the recorded remote directory is not re-entered in this
version, and nothing that was running is re-run). A host that no longer resolves
opens a local shell instead; a local pane whose recorded directory has vanished
or denies access retries at your home directory rather than aborting the
restore. Snapshots saved before remote restore existed reopen those panes as
local shells.

A session can also be saved as a **named layout**. A layout captures the **whole
application**: every workspace, with its tabs, splits, working directories, and
host bindings. Save one via **Save as Layout…** on the content-grid right-click
menu, a workspace rail slot's right-click menu, the empty rail menu, or the
command palette (**Save All Workspaces as Layout**). A single workspace can be
captured on its own with **Save Workspace as Layout…**: a workspace rail slot's
right-click menu saves the clicked workspace, the content-grid menu saves the
active one, and the palette entry (**Save Workspace as Layout**) saves the
active one. Saving under a name that already exists prompts before overwriting,
so you can replace the existing layout, pick a different name, or cancel.

Opening a layout later (**Open Layout** in the palette, or **Open Layout…** from
the empty rail, the empty tab strip, or the content-grid menu) asks how it should
land when the window already holds real state:

- **Replace** tears down the current workspaces and installs the saved set as the
  whole window.
- **Add** appends the saved workspace(s) beside the current ones.
- **Cancel** leaves everything untouched.

A fresh window that still holds a single untouched default workspace skips the
prompt: the opened layout consumes that workspace so the window shows exactly
what was saved. With no layouts saved yet the picker explains how to create one.

On Unix, a restored or instantiated pane whose detached session-host is still
alive reattaches to it; a dead one silently opens a fresh shell, with a compact
"N of M sessions reattached" notice. Restore and named layouts are
cross-platform (the state dir uses `%LOCALAPPDATA%` on Windows); session-host
reattach is Unix-only, so a Windows restore always lands fresh shells.

Any tab can be split into panes. The direct chords `Ctrl+Shift+E` (split into
columns, new pane on the right) and `Ctrl+Shift+O` (split into rows, new pane
below) create a split on a single-pane tab; they match Ghostty's Linux
defaults and work at both single-pane and multi-pane. You can also split from
the terminal's right-click menu's "Split Right" / "Split Down" items. When the
active tab is already multi-pane, that same content menu also offers a "Close
Pane" item (labelled with the effective `Ctrl+b x` prefix chord) to close just
the focused pane; it is hidden in a single-pane tab, where closing the tab is
the only close. The content menu also has a launcher section at the bottom:
"Connection Manager", "Command Palette", and "Session Replay", each labelled
with its effective chord and opening the matching overlay. Right-clicking a tab
opens a separate, tab-scoped menu (New Tab, Rename Tab, Close Tab, Close Other
Tabs, **Connect to Host…**, **Replace with Host…**, New Window, plus Move to
Workspace… when more than one workspace exists), and right-clicking the empty
tab strip offers New Tab, Command Palette, and Settings. **Connect to Host…**
opens a saved host in a new tab positioned right after the clicked one (the
clicked shell is left untouched); **Replace with Host…** opens the host in the
clicked tab's place, first asking to confirm when that tab still has a program
running. Once the active tab
has multiple panes, a tmux-style prefix (default
`Ctrl+b`, configurable via
`pane_prefix`) opens a transient pane-command mode; press the prefix then a pane
key:

| After the prefix | Action |
| --- | --- |
| `%` | Split the focused pane into columns (side-by-side) |
| `"` | Split the focused pane into rows (stacked) |
| `←` / `→` / `↑` / `↓` | Move focus to the neighbor pane |
| `o` | Cycle focus to the next pane |
| `x` | Close the focused pane |
| `z` | Zoom / un-zoom the focused pane (full-bleed; layout is preserved) |
| `Space` / `=` | Equalize split sizes |
| `Ctrl+b` (prefix again) | Send a literal prefix to the focused pane (nested multiplexer) |

Each pane owns an independent PTY, terminal model, scrollback, viewport,
selection, search, and cursor. Drag a divider to resize the panes on either
side. The prefix is captured only when the active tab has more than one pane; a
single-pane shell receives `Ctrl+b` unchanged, preserving the byte-identical
default input path. Set `pane_prefix=off` to disable the pane prefix entirely
and free the chord in multi-pane tabs too. v1 cuts: inline
graphics render in single-pane tabs only, interactive overlays (selection /
search) are painted for the focused pane only. Optional inactive-pane dimming is
implemented via `inactive_pane_dim`; it defaults to `0.0`, is disabled on
`render_quality=plain`, and leaves the no-dim pane frame byte-identical.

Core local shortcuts:

| Shortcut | Action |
| --- | --- |
| `Ctrl+Shift+E` / `Ctrl+Shift+O` | Split the focused pane into columns / rows |
| `Ctrl+Shift+F` | Search scrollback |
| `Ctrl+Shift+,` | Settings panel |
| `Ctrl+Shift+H` | Theme picker |
| `Ctrl+Shift+B` | Theme builder |
| `Ctrl+Shift+P` | Command palette |
| `Ctrl+Shift+S` | Connection manager |
| `Ctrl+Shift+R` | Session replay |
| `Ctrl+Shift+A` | Manage Sessions (attach a detached session) |
| `Ctrl+Shift+C` / `Ctrl+Shift+V` | Copy / paste |
| `Shift+PageUp` / `Shift+PageDown` | Scroll local viewport |
| `Ctrl+Shift+L` | Keyboard quick-select hints |
| `Ctrl+Shift+Space` | Keyboard copy mode |
| `Ctrl+Shift+Up` / `Ctrl+Shift+Down` | Jump to previous / next prompt mark |
| `Ctrl+Shift+K` | Clear editable prompt input when shell integration allows it |
| `Delete` / `Backspace` | Delete selected editable prompt input when shell integration allows it |

The command palette, connection manager, session replay, theme builder, and
Manage Sessions each ship with a default `Ctrl+Shift+<letter>` chord and a
discoverable menu entry: the launcher actions appear in the right-click menu's
launcher section, and the theme builder is an "Open Theme Builder" entry in the
Settings → Themes section. These chords are all `Ctrl+Shift+<letter>`, which a
TUI cannot receive, so PTY input is unchanged. Prompt navigation is the
`Ctrl+Shift+Up/Down` arrows. Rebind any of them, for example:

```conf
# odytty.conf
keybinds = ctrl+alt+p=command-palette
```

## Shell Integration

Some prompt-aware actions need OSC 133 prompt marks from the shell:
prompt jumps, clearing/deleting editable prompt input, command-status gutters,
and click-to-position support when the shell advertises it. OdyTTY parses these
marks by default but does not inject hooks unless you opt in.

Set `shell_integration = on` in Settings or `odytty.conf` to make newly spawned
local `bash`, `zsh`, and `fish` shells load OdyTTY's integration wrapper at
startup. The wrapper sources your normal shell config first, then installs the
OSC 133 hooks. Existing shells are unchanged until restarted. Bash integration
uses an interactive `--rcfile`, so login-shell-only startup files remain your
shell's responsibility. On Windows, a `powershell`/`pwsh` shell is injected with
an OdyTTY PowerShell profile via `-NoExit -Command` (PSReadLine drives the
command-start mark); `cmd.exe` has no OSC 133 hook surface and is unsupported.

For manual setup, SSH/login shells, or users who prefer explicit rc edits, print
the snippet and source it yourself:

```sh
eval "$(odytty shell-integration bash)"
eval "$(odytty shell-integration zsh)"
odytty shell-integration fish | source
```

Until prompt input marks are active, the right-click menu disables Cut/Delete
for prompt input and shows an "Enable shell integration in Settings" hint. A
plain `Delete` / `Backspace` with **no** selection still passes through to the
shell normally; with an active selection but no known prompt boundary, OdyTTY
will not send blind edit bytes; it clears the stale selection and surfaces the
same shell-integration hint instead of risking a corrupted command line.

For a one-off/dev override, run
`ODYTTY_KEYBINDS="ctrl+alt+p=command-palette" odytty`; env wins for that
session.

The palette fuzzy-filters local actions, bounded read-only shell history, and
recent OSC 7 directories. Selecting a history or directory row types that text
into the active pane without pressing Enter; selecting an action runs the local
action after the overlay closes.

Output replay is available as the `session-replay` action, bound by default to
`Ctrl+Shift+R`. Turn on recording with `session_replay = on` (or
`ODYTTY_SESSION_REPLAY=on` as a one-off override); it is off by default, so the
plain path is unchanged. Then rebind the scrub overlay if desired:

```conf
# odytty.conf
session_replay = on
keybinds = ctrl+alt+r=session-replay
```

For a one-off/dev override, run
`ODYTTY_SESSION_REPLAY=on ODYTTY_KEYBINDS="ctrl+alt+r=session-replay" odytty`;
env wins for that session.

The ring is capped (600 frames and 24 MiB, whichever binds first) and
local-only; frames never touch disk or the network. The overlay is
presentation-only: `←`/`→` step, `PgUp`/`PgDn` jump ten, `Home`/`End` jump to
the ends, and the live session keeps running underneath untouched while you
scrub.

The connection manager is available as the `connection-manager` action, bound by
default to `Ctrl+Shift+S`. Rebind it to open a type-to-filter list of saved
hosts and quick-connect with Enter:

```conf
# odytty.conf
keybinds = ctrl+alt+h=connection-manager
```

For a one-off/dev override, run
`ODYTTY_KEYBINDS="ctrl+alt+h=connection-manager" odytty`; env wins for that
session.

Hosts come from the OdyTTY-owned `hosts.conf` and, only when
`ssh_config_hosts = on`, name-only entries from your OpenSSH config. With the
opt-in off, the overlay lists OdyTTY-owned hosts only and never references
`~/.ssh`. The overlay is presentation-only; selecting a host spawns the system
`ssh` in a new session.

**Ad-hoc connect.** You do not have to save a host first. When the filter query
matches no saved host but is a well-formed `[user@]host[:port]`, the overlay
offers a **Connect to: …** row: **Enter** connects to it straight away (through
the same path a saved host uses), and **Shift+Enter** (or **Ctrl+S**) connects
*and* appends a matching `Host` block to `hosts.conf` so it is saved for next
time. The write is atomic and preserves the file's existing contents; if the
alias already exists it just connects and reports "already saved". Typed input
is validated: embedded spaces, a leading `-`, or an out-of-range port are
rejected so nothing option-injects into the `ssh` command line.

**Add / Edit form.** For more than a quick save, the connection manager opens an
in-app form: **Tab** starts a blank **Add connection** and the right arrow opens
an **Edit** pre-filled from the selected OdyTTY-owned host (`ssh-config`-imported
rows are read-only). Alongside alias / host / user / port, an **Advanced** section
carries an `IdentityFile` path (adds `ssh -i` on connect; a path, never a stored
secret; `ssh-copy-id` is the once-and-done way off passwords), the three-way
`Integration` / `Reuse` / `Tmux` overrides (**inherit / on / off**), and
theme / font / title. Saving appends a new block or edits the existing one in
place, leaving every other block, comment, and unknown field byte-for-byte
untouched. A **Test connection** button runs a non-interactive background probe
and reports an honest tri-state result without ever handling a password:
reachable with key/agent auth, reachable but interactive-auth (expected for a
password host; the connect still works), a host-key mismatch, or unreachable.

**Host-row right-click menu.** Right-clicking a saved-host row opens a small menu
with **Open in New Tab** (connect in the current workspace), **Open in New
Workspace** (a fresh workspace, pre-bound to the host so its new tabs open there
too), and **Bind Current Workspace** (route the active workspace's new tabs
through this host). For OdyTTY-owned rows it also offers **Edit…** (the same
pre-filled form) and **Remove…** (deletes the host's `hosts.conf` block after a
confirm); `ssh-config`-imported rows are read-only, so those two are hidden.
Dismissing the menu returns to the manager with its selection intact.

**Remote shell integration.** By default (`remote_integration`, on) connecting
to a saved host carries OdyTTY's shell integration onto the remote: an inline,
bash-only bootstrap writes a temporary rcfile on the remote and execs an
interactive shell against it, so a remote bash session gains the same prompt
and input boundaries as a local one. Nothing is persisted on the remote, and a
non-bash shell or any failure degrades to a plain `ssh`. The tab is titled
`user@host`. `remote_reuse` (on) multiplexes further tabs to the same host over
a shared ControlMaster connection so they connect with no new handshake, and
`remote_persist` (10 minutes by default) keeps that master socket alive after
the last tab closes so a quick reconnect skips re-authentication;
`remote_tmux` (off by default) wraps the remote shell in a persistent
`tmux new-session -A -s odytty` so a dropped-and-reconnected link resumes the
same remote session. When a remote connection drops, the tab is held open with
a reconnect prompt: **Enter** reconnects in place, **Esc** or **Ctrl+D**
closes the tab. Pasting a clipboard image into an integrated remote tab offers
a confirm-first upload (`remote_image_paste`, `ask` by default). A one-line
prompt appears in the pane (`upload image <size> to user@host?  Enter: upload
· Esc: cancel`), and only on **Enter** does the image transfer. It streams over
the same authenticated `ssh` connection (reusing the ControlMaster socket when
one is up) into a `0600` file under an unguessable `/tmp` name; nothing runs
remotely and images above the 10 MiB cap are refused with a notice. On success
the pane shows `image uploaded <path> · copied to clipboard` and the remote
path is copied to the local clipboard; it is **not** typed into the shell, so
an empty prompt never runs a stray path. Paste it (`Ctrl+Shift+V`) wherever you
want it as a command argument. Uploaded files are cleaned up best-effort when
the tab closes. This works on reconnected and restored remote tabs too. Each
knob has a per-host override in
`hosts.conf` (`Integration`, `Reuse`, `Tmux`). Connection reuse is a
Unix-client feature; on a Windows client each connection authenticates
independently through `ssh.exe`.

Detached sessions are managed from inside the window as well as from the CLI. The
`session-attach` action (default `Ctrl+Shift+A`, also the **Manage Sessions**
entry in the right-click menu) lists the live detached sessions; selecting one
attaches it. If that session is already open it switches to its tab; otherwise a
prompt offers a **New tab** or **Replace** (which closes the current tab).
Right-click a session to kill it behind a confirmation, and the **Detach &
switch** context-menu action hands the focused pane's working directory to a
fresh managed session. Attaching reconnects the live PTY and terminal model; the
host keeps them alive across detach/attach cycles until the child exits or the
idle timeout reaps it.

Interactive paths are an opt-in layer (`interactive_paths`, off by default) that
makes file paths in command output actionable. With it on, Ctrl+click opens a
path: a file opens in your configured editor, jumping to the right `line:col`
when the path carries one, and an image path (png/jpg/jpeg/webp) opens in an
in-app lightbox you dismiss with `Esc` or a click outside. A discoverable click
hint, plus right-click "Open", "Open With…", "Copy Path", "Copy File", and
"Reveal in File Manager", round out the menu. Opening goes through the platform
opener (`xdg-open` on Linux, `open` on macOS) with a scheme allowlist; nothing is
ever run through a shell.

The `keybinds` config key can rebind local actions. The global actions are
`search`, `settings`, `theme-picker`, `theme-builder`, `copy`, `paste`,
`scroll-up`, `scroll-down`, `jump-prompt-prev`, `jump-prompt-next`, `copy-mode`,
`hints`, `clear-input`, `command-palette`, `session-replay`,
`connection-manager`, `session-attach`, `new-tab`, `new-window`, `next-tab`,
`prev-tab`, `close-tab`, and `duplicate-tab`. The workspace actions are
`new-workspace`, `close-workspace`, `rename-workspace`, `next-workspace`,
`prev-workspace`, and `workspace-picker`. The pane actions (`split-columns`,
`split-rows`, `focus-pane-left` / `-right` / `-up` / `-down`, `focus-pane-next`,
`close-pane`, `zoom-pane`, `equalize-panes`) are rebindable too; the chord is the
key pressed *after* the prefix, for example `keybinds = ctrl+f=zoom-pane`.
`ODYTTY_KEYBINDS` provides the same syntax as a session-scoped override. See
[`docs/keybindings.md`](docs/keybindings.md) for the complete keyboard reference:
every default chord, the pane prefix, copy mode, hints, and rebinding.

## Settings And Themes

Settings load in this order:

1. Built-in defaults.
2. `$XDG_CONFIG_HOME/odytty/odytty.conf`, or
   `~/.config/odytty/odytty.conf`.
3. `ODYTTY_*` environment variables, which override config values for the
   current session.

The config file is the primary place to set durable preferences. Its format is
`key = value` with `#` comments. The native app polls the resolved file about
once per second; env-pinned keys stay pinned for the session and are best suited
to one-off/dev overrides. The settings panel live-applies changes and writes
only changed keys back to `odytty.conf`, preserving comments, blank lines,
unknown keys, and ordering via same-directory atomic rename.

`theme = system` or `ODYTTY_THEME=system` follows the desktop dark/light
preference using OdyTTY defaults (`odyssey` dark, `odyssey-light` light).
Explicit `follow_os_theme`, `os_theme_dark`, and `os_theme_light` settings allow
custom mappings.

### Default background image (and how to turn it off)

Since v0.6.0 OdyTTY ships with its OdysseyOS visual identity on by default: the
default theme is `odyssey-default` (a deep forest-green palette; also reachable
under the `odyssey-jungle` alias), and an original "Dark Waves" background image
is **bundled into the binary and shown by default** behind the grid. The image
is embedded at build time, so it works identically on every install (source
build, AppImage, and distro package) with no external file to manage. It carries
the repository license (see [`assets/backgrounds/LICENSE`](assets/backgrounds/LICENSE)).

To turn the background **off**, set either key in `odytty.conf`:

```ini
# odytty.conf: disable the bundled background entirely
background_treatment = color   # draw the theme background only (no image)
# or use:
background_image = none        # keep image treatment available but use no image
```

To use **your own** image instead, point `background_image` at a file:

```ini
# odytty.conf: use a custom background
background_treatment = image
background_image = /path/to/your/wallpaper.png   # png / jpeg / webp
background_image_scrim = 0.5                      # 0 = none, 1 = opaque scrim; `auto` = floor-safe
```

`background_image = default` (the unset value) selects the bundled image again.

### Window transparency

OdyTTY can render its background translucent so the desktop shows through the
terminal. Text, the cursor, selection, and every overlay (menus, pickers, the
settings panel) always stay fully opaque; the readability boundary is hard, so
only the background fades. It is **off by default**; the opaque path is
unchanged. Enable it from the settings panel (Rendering → Window transparency /
Window opacity) or `odytty.conf`:

```ini
# odytty.conf: let the desktop show through the terminal background
window_transparency = on
window_opacity = 85          # percent, 20..=100 (step 5); 100 is fully opaque
```

Transparency needs a compositing window manager: Wayland handles it natively,
X11 needs a compositor running, and Windows uses DWM. Where the display server
offers no alpha compositing the toggle has no visible effect. A menu, picker, or
the settings panel stays a readable opaque surface while it is open, and only
that panel, not the whole window. The terminal behind it keeps showing the
desktop through, so opening a menu no longer flashes the window opaque.

A configured background image is part of that background: with transparency
on it becomes translucent too and composes over the desktop, rather than
sealing the window opaque where the image draws.

See:

- [`docs/runtime-knobs.md`](docs/runtime-knobs.md) for every config key,
  environment variable, range, default, and reload behavior.
- [`docs/odytty.conf.example`](docs/odytty.conf.example) for an annotated config.
- [`docs/themes.md`](docs/themes.md) for the theme format and built-in roster.
- [`docs/effects.md`](docs/effects.md) for bloom, CRT, retro, background, and
  motion effects.
- [`docs/keybindings.md`](docs/keybindings.md) for the complete keyboard
  reference and rebinding.
- [`docs/accessibility.md`](docs/accessibility.md) for the minimum-contrast
  floor, color-vision modes, dimming, and the bell.
