# OdyTTY

**Website:** [odytty.unfinished-works.com](https://odytty.unfinished-works.com)

![OdyTTY rendering a colorized git graph, project tree, and truecolor gradients under the default Odyssey theme with bloom](assets/demo.png)

OdyTTY is a standalone, from-scratch, GPU-rendered Rust terminal emulator for
Linux, macOS, and Windows. It owns the terminal byte path from PTY allocation through escape
parsing, terminal state, render geometry, and shaders, while relying on focused
external crates for lower-level infrastructure such as `wgpu`, `winit`,
`ab_glyph`, `swash`, `arboard`, and Unicode width tables.

The name and visual direction come from OdysseyOS, a companion Linux From
Scratch system. That system is inspiration, not a platform
requirement: OdyTTY is a public Linux application and does not require
OdysseyOS or any custom distribution.

The project goal is not to skin an existing terminal. OdyTTY is testing whether
a terminal can carry a distinctive OdyTTY visual identity, richer in-app
configuration, inline media, motion, and accessibility features while remaining
practical for real command-line work. Terminal correctness, readable text,
input behavior, stable rendering, local privacy, and performance are the hard
floor.

OdyTTY is in active development. It is already a broad prototype: a native
window opens real local shells, supports multiple sessions with a tab bar,
splits each tab into panes, renders text and inline graphics on the GPU, and has
a substantial compatibility and smoke-test suite. It is still Linux-first and
pre-release; macOS is supported as an experimental build-from-source target, and
Windows ships as an unsigned portable zip.

## Highlights

- **Owned terminal core:** Linux PTY layer via `rustix`, clean-room DEC/xterm
  parser, OdyTTY terminal model, scrollback, alternate screen, mouse, keyboard,
  OSC, DCS, APC, and render geometry.
- **GPU renderer:** `wgpu`/Vulkan path with dynamic glyph atlas, bundled
  Victor Mono (default) plus JetBrains Mono, bold/italic/weight faces, optional
  synthetic styles, subpixel AA, HiDPI-aware atlas rebuilds, color emoji atlas,
  and GPU image layer.
- **Inline media:** Kitty graphics protocol and Sixel, including direct,
  file/temp-file, and shared-memory Kitty transports with conservative local
  file-safety restrictions.
- **Modern input:** Kitty keyboard protocol, SGR pixel mouse mode 1016, focus
  reporting, IME composition input (CJK / compose-key accents), configurable
  local keybindings, keyboard hints, keyboard copy mode, and OSC 8 hyperlink
  hover/open.
- **Daily workflow:** search, refined selection, PRIMARY selection,
  bracketed-paste hardening, chunked large paste, bounded scrollback history,
  right-click context menu,
  command-aware prompt navigation from OSC 133, click-to-place cursor (with
  shell integration, click in the typed command line to move the shell cursor
  there — including across soft-wrapped lines), configurable bell (visual flash
  / window urgency), close confirmation, and tabs.
- **Splits / panes:** split any tab into side-by-side or stacked panes, each
  with its own shell, scrollback, selection, search, and cursor. A configurable
  tmux-style prefix (default `Ctrl+b`) drives split, focus-move, close, zoom
  (full-bleed the focused pane), and equalize once a tab has multiple panes;
  dividers are drag-resizable. On a single-pane tab, `Ctrl+b` passes through to
  the shell unchanged, and a single-pane tab renders exactly as before.
- **Clickable paths and inline media:** opt-in interactive paths turn file paths
  in output into Ctrl+click targets that open in your editor at the right line
  and column, with an in-app image lightbox for image paths, an "Open With…"
  picker, OSC 8 hyperlinks, and keyboard quick-select hints.
- **Detached and managed sessions:** sessions that outlive the window — a local
  CLI (`odytty new` / `list` / `attach`) plus an in-window Manage Sessions
  overlay to attach, rename, and kill them, a New-tab/Replace attach prompt, and
  detach-and-switch to hand the focused pane to a fresh managed session.
- **Visual experience layer:** 100 built-in themes, user `.theme` files, live theme
  picker, theme builder, semantic cursor/selection/search roles, optional
  bloom/CRT/retro effects, background treatments, cursor motion, focus dimming,
  new-output fade, window padding, and window border.
- **Local configuration UX:** `odytty.conf`, live reload, in-app settings panel,
  atomic preservation-first writeback, mouse-friendly controls, font picker,
  keybinding editor, and first-run onboarding. Environment variables always win.
- **SSH connection substrate:** an OdyTTY-owned local hosts list for the SSH
  manager, default-off opt-in OpenSSH config host-name import, and a connect
  action that opens `ssh` in a new tab/session. Imports are read-only,
  name-only, bounded, and never surface key material; authentication remains
  with the system `ssh` binary and agent. Connections can optionally carry
  OdyTTY's shell integration onto the remote, reuse a single authenticated
  connection across tabs, hold a dropped tab open with a reconnect prompt, and
  persist the remote shell in `tmux` — each an opt-in knob, all degrading to a
  plain `ssh` when unavailable.
- **Privacy posture:** no telemetry, analytics, crash reporting, account,
  cloud sync, or update ping. Network-capable actions are explicit and
  user-initiated: Ctrl-click link and path opening through the platform opener
  (`xdg-open` on Linux, `open` on macOS) with a scheme allowlist, and SSH
  connect entries that exec the system `ssh` binary.

## Install And Run

Linux is the primary, battle-tested target. macOS is an experimental
build-from-source target (see [macOS](#macos-experimental) below). Windows ships
as an unsigned portable zip; Scoop is the recommended install path, or download
the zip directly.

### Linux

Requires Linux and a Vulkan-capable GPU. Wayland is the primary target; X11
works through the current `winit`/GPU stack with some window-manager-dependent
behavior for borderless windows and OS theme detection.

#### AppImage (quickest)

Download the AppImage and `SHA256SUMS` from the latest release, verify it, make
it executable, and run it:

```sh
curl -LO https://github.com/ghreprimand/odytty/releases/latest/download/odytty-x86_64.AppImage
curl -LO https://github.com/ghreprimand/odytty/releases/latest/download/SHA256SUMS
sha256sum -c SHA256SUMS --ignore-missing
chmod +x odytty-x86_64.AppImage
./odytty-x86_64.AppImage
```

The `chmod +x` is required — browsers download the AppImage without the
executable bit, so without it the file opens in an archive viewer or fails with
"permission denied". The AppImage needs a working host Vulkan driver (Mesa or a
vendor driver); it deliberately does not bundle the GPU stack. This is a
best-effort x86_64 artifact — if it fails to start, build from source below.

Each release attaches both version-less and version-pinned downloads:
`odytty-x86_64.AppImage` / `odytty-windows-x86_64.zip` / `odytty.tar.gz` are
the always-latest names used above (resolved by the `releases/latest/download/`
URLs), while `odytty-<version>-x86_64.AppImage` /
`odytty-<version>-windows-x86_64.zip` / `odytty-<version>.tar.gz` are the
**identical** copies for pinning a specific version. Each alias and its
version-pinned twin carry matching checksums in `SHA256SUMS`.

#### Build from source

For the current source release, install OdyTTY for the current user.

Download and verify the release archive:

```sh
workdir=$(mktemp -d /tmp/odytty-install.XXXXXX)
cd "$workdir"
curl -LO https://github.com/ghreprimand/odytty/releases/latest/download/odytty.tar.gz
curl -LO https://github.com/ghreprimand/odytty/releases/latest/download/SHA256SUMS
grep " odytty.tar.gz$" SHA256SUMS | sha256sum -c -
tar -xf odytty.tar.gz
cd odytty-*/
```

Build the release:

```sh
cargo build --release --locked
```

Install a versioned binary and point `~/.local/bin/odytty` at it:

```sh
install -Dm755 target/release/odytty "$HOME/.local/opt/odytty/$version/bin/odytty"
mkdir -p "$HOME/.local/bin"
ln -sfn "$HOME/.local/opt/odytty/$version/bin/odytty" "$HOME/.local/bin/odytty"
```

Register the app launcher, metadata, and icon:

```sh
install -Dm644 dist/linux/io.unfinished_works.odytty.desktop \
  "$HOME/.local/share/applications/io.unfinished_works.odytty.desktop"
install -Dm644 dist/linux/io.unfinished_works.odytty.metainfo.xml \
  "$HOME/.local/share/metainfo/io.unfinished_works.odytty.metainfo.xml"
install -d "$HOME/.local/share/icons/hicolor"
cp -a dist/icons/hicolor/* "$HOME/.local/share/icons/hicolor/"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
gtk-update-icon-cache "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
```

Make sure `$HOME/.local/bin` is on `PATH`, then launch OdyTTY as a normal
application:

```sh
odytty
```

### Windows

Windows support is new and still maturing — treat it as an early, actively
supported target rather than a settled one. It builds and runs the full
terminal, and every push is exercised on a Windows CI leg, but the polish bar
is behind Linux. **Bug reports for the Windows build are especially welcome** —
please [open an issue](https://github.com/ghreprimand/odytty/issues) with your
Windows version and a short repro if something misbehaves.

The Windows build is a single unsigned, portable `odytty.exe` packaged as
`odytty-windows-x86_64.zip`. There is no installer and nothing is written
outside your profile — configuration lives under `%APPDATA%\odytty\`. Two
install paths:

**Scoop (recommended).** [Scoop](https://scoop.sh) is a per-user package
manager for Windows — no admin rights, everything under your profile. If you
don't already have it, install it once in PowerShell:

```powershell
Set-ExecutionPolicy -ExecutionPolicy RemoteSigned -Scope CurrentUser
Invoke-RestMethod -Uri https://get.scoop.sh | Invoke-Expression
```

Then add the OdyTTY bucket and install:

```powershell
scoop bucket add odytty https://github.com/ghreprimand/odytty
scoop install odytty
```

Scoop puts `odytty` on your PATH (a shim under `~\scoop\shims`), so you can
launch it by typing `odytty` in any shell, and it adds an **OdyTTY** entry to
the Start menu. It also verifies the download against the release checksum and
unpacks the zip for you, so the browser "downloaded from the internet" file
warning never comes up. Update later with `scoop update odytty`.

**Portable zip (direct download).** Download the always-latest zip and checksum
file, verify the hash, unpack, and run:

```powershell
Invoke-WebRequest https://github.com/ghreprimand/odytty/releases/latest/download/odytty-windows-x86_64.zip -OutFile odytty-windows-x86_64.zip
Invoke-WebRequest https://github.com/ghreprimand/odytty/releases/latest/download/SHA256SUMS -OutFile SHA256SUMS
Get-FileHash odytty-windows-x86_64.zip -Algorithm SHA256
Expand-Archive odytty-windows-x86_64.zip -DestinationPath .\odytty
.\odytty\odytty.exe
```

Compare the printed hash against the `odytty-windows-x86_64.zip` row in
`SHA256SUMS`; they must match before you run the binary. Put `odytty.exe`
anywhere on your `PATH` (for example `%LOCALAPPDATA%\Microsoft\WindowsApps`) to
launch it as `odytty` from any shell; right-click it in Explorer to pin it to
Start or the taskbar for an easy launcher.

**About the SmartScreen prompt.** OdyTTY is not code-signed yet, so the first
time you launch it Windows may show a blue "Windows protected your PC"
SmartScreen dialog naming an unknown publisher. This is expected for unsigned
open-source software and can appear however you install it — a package manager
removes the browser-download friction but does not, on its own, guarantee the
first-run prompt won't show. To run OdyTTY: click **More info**, then **Run
anyway**. To clear the "downloaded from the internet" mark up front instead,
unblock the unpacked exe in PowerShell before launching:

```powershell
Unblock-File .\odytty\odytty.exe
```

Code-signed Windows binaries are a planned improvement; until then the
SmartScreen click-through above is the intended one-time step.

**Not yet a Windows "default terminal."** You launch OdyTTY directly — from the
Start menu, by typing `odytty`, or from a pinned shortcut — and it hosts your
shells in its own tabs and panes. It can't yet be set as the system *default
terminal* (the app Windows hands console programs to when they're launched from
Explorer or another program): that requires implementing the Windows
default-terminal handoff protocol, which OdyTTY doesn't do yet. It's tracked as
future work.

**Windows scope (v1).** The Windows build opens local ConPTY-backed tabs and
panes with the full rendering, theme, effect, and inline-graphics stack, plus
persistent configuration under `%APPDATA%`, host-font discovery from
`C:\Windows\Fonts`, Windows clickable-path detection (drive-letter and UNC),
default-open/reveal via `cmd`/`explorer`, and SSH-in-a-tab over the local
pseudoconsole. Not in this release: detachable/resumable session hosting and
detached SSH (Unix-only), the headless `--interactive` mode, and the full
"Open With" application list; the hostname field and command-palette shell
history degrade to empty on Windows. Interactive Windows behaviour is verified
manually on-device — CI proves the build compiles and its unit tests pass.

### macOS (experimental)

macOS is experimental and not yet as battle-tested as Linux. OdyTTY is built
from source: a binary you compile locally is never quarantined, so it launches
with no Gatekeeper warning and needs no signing, notarization, or workaround.
There is no prebuilt download — and that is deliberate, since an unsigned
prebuilt would otherwise be blocked by Gatekeeper on first launch.

Requires the Rust toolchain ([rustup](https://rustup.rs)) and the Xcode Command
Line Tools. Both Apple Silicon and Intel are supported (whatever `cargo` targets
natively on your machine).

```sh
xcode-select --install   # once, if you don't already have the Command Line Tools
curl -LO https://github.com/ghreprimand/odytty/releases/latest/download/odytty.tar.gz
curl -LO https://github.com/ghreprimand/odytty/releases/latest/download/SHA256SUMS
grep " odytty.tar.gz$" SHA256SUMS | shasum -a 256 -c -
tar -xf odytty.tar.gz
cd odytty-*/
cargo build --release --locked
./target/release/odytty
```

To get a double-clickable **OdyTTY.app** in Applications (a locally built bundle
is also not quarantined, so it opens with no Gatekeeper prompt):

```sh
mkdir -p dist/build
cp target/release/odytty dist/build/odytty
bash dist/macos/make-app.sh "$version"
cp -R dist/build/OdyTTY.app /Applications/
```

To run it as just `odytty` from any shell (as the examples below show), symlink
the built binary onto your `PATH`:

```sh
mkdir -p "$HOME/.local/bin"
ln -sfn "$PWD/target/release/odytty" "$HOME/.local/bin/odytty"
# Make sure ~/.local/bin is on PATH (add to ~/.zshrc if needed):
#   export PATH="$HOME/.local/bin:$PATH"
```

Run a command directly inside OdyTTY:

```sh
odytty -e btop
odytty --working-directory /tmp -e sh -lc 'pwd; exec "$SHELL"'
odytty --title Monitor -e btop
```

Useful launch examples:

```sh
# Use the hard plain renderer profile.
ODYTTY_RENDER_QUALITY=plain odytty

# Follow the desktop dark/light preference with OdyTTY defaults.
ODYTTY_THEME=system odytty

# Larger text with a named system font.
ODYTTY_FONT_SIZE=24 ODYTTY_FONT_FAMILY="DejaVu Sans Mono" odytty

# RGB subpixel antialiasing when supported by the GPU.
ODYTTY_SUBPIXEL=rgb odytty

# Stronger phosphor reference look.
ODYTTY_RETRO=on odytty
```

For system installs, Odyssey/LFS packaging, rollback, and default-terminal
notes, see [`docs/install.md`](docs/install.md). A quick source-tree smoke run
is:

```sh
cargo build --release --locked
./target/release/odytty
```

OdyTTY is driven primarily by its in-app menus: the settings panel, the theme
and font pickers, and the `Ctrl+Shift+P` command palette all run live inside the
terminal, so browsing themes, choosing fonts, and changing configuration happen
visually with an immediate preview — no command-line required. The introspection
commands below are a **scriptable alternative** to those menus for quick checks,
automation, and headless inspection; they print and exit without opening a
window:

```sh
odytty --list-themes
odytty --list-fonts
odytty --show-config
```

Detached-session CLI commands are additive and local-only:

```sh
odytty new --detached
odytty new --detached --title work -e bash
odytty list
odytty attach
odytty attach <id>
odytty attach --diagnostic <id>
```

The same introspection works from the source tree before installing:

```sh
./target/release/odytty --list-themes
./target/release/odytty --list-fonts
./target/release/odytty --show-config
```

`--list-themes` prints the 100 built-in themes as stable
`name`/`appearance`/`family` rows. `--list-fonts` prints discoverable system
font files. `--show-config` prints the current stable config-dump subset (including
`symbol_fallback` and `symbol_font_source`, which reports the resolved
symbol/Nerd-font fallback **chain**, joined with ` > ` — e.g.
`bundled > bundled > host:<path>`, or `disabled`); the full settings authority is
[`docs/runtime-knobs.md`](docs/runtime-knobs.md).

`odytty new --detached` starts a local session-host process and prints a stable
`id=...` row. `odytty list` prints live detached sessions as metadata-only rows
with the session name first, pane count, age, and id when the name is distinct;
it never dumps scrollback or command output. `odytty attach` with no id attaches
the only live session; if several are live, it prints the same readable list and
asks you to choose `odytty attach <id>`; if none are live, it reports that there
is nothing to attach. `odytty attach <id>` reattaches a detached session in a
live native window: the window opens its normal local shell, adds the hosted
session as a focused tab, repaints from the host snapshot, then streams live
output. If the id is missing or dead, the local shell still opens and stderr
reports `odytty: attach session <id> failed: <err>`. `odytty attach
--diagnostic <id>` is the script/CI form: it prints a one-line status dump and
exits without opening a window. The host keeps the PTY and bounded terminal
model alive across attach/detach cycles until the child exits or the detached
idle timeout reaps it.

## Current Feature Surface

### Terminal Compatibility

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
screen, so full-screen TUIs that do not track the mouse — pagers and similar —
scroll with the wheel.

Keyboard support includes mode-aware legacy encoding and the Kitty keyboard
protocol as a negotiated overlay. With no Kitty flags active, legacy bytes are
preserved. IME composition is enabled: input-method pre-edit is rendered inline
at the cursor and committed text is sent to the shell, so CJK input methods and
compose-key/dead-key accents work.

The bell (BEL) is configurable via the `bell` setting: `urgent` (default,
requests window attention when unfocused), `visual` (a brief readability-safe
screen flash), `all` (both), or `off`. There is no audible bell.

### Text, Emoji, And Graphics

Text rendering uses bundled Victor Mono by default at 20 logical pixels with
line height `1.0`. JetBrains Mono is also bundled and remains selectable via
`font_family`. The font picker groups families into **Bundled Fonts** (Victor
Mono, JetBrains Mono — always available) and **System Fonts** (host monospace
families), and either resolves with zero config. The symbol/Nerd-font fallback
is a **chain** of bundled faces — Nerd Fonts **v3** and **v2** — so PUA prompt
icons render out of the box regardless of which Nerd Font era a config emits or
whether the host has any Nerd font installed. System font families, direct font
files, font-weight variants, per-range symbol maps, synthetic styles, subpixel
AA, glyph coverage gamma, stem darkening, and minimum-contrast enforcement are
configurable.

Color emoji uses `swash` and a dedicated premultiplied-RGBA atlas. Bitmap-strike
color fonts are supported — Noto Color Emoji (CBDT/CBLC) on Linux and Apple Color
Emoji (sbix) on macOS — including variation selectors, flags, keycaps, skin
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

### Native App Workflow

The native app runs multiple sessions. `Ctrl+Shift+T` opens a new tab,
`Ctrl+Shift+W` closes the active tab, and `Ctrl+PageDown` /
`Ctrl+PageUp` switch tabs. Closing a tab closes the **whole** tab — every pane
it holds — which is distinct from closing a single pane (see "Close Pane"
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
session looks exactly as before — no extra chrome. Once a second workspace
exists, a vertical **rail** lists them down one side; `Ctrl+Shift+PageDown` and
`Ctrl+Shift+PageUp` cycle between workspaces. `Ctrl+Shift+Enter` creates a new
workspace and `Ctrl+Shift+G` opens the workspace picker. The rail's `+` slot
also creates a new workspace, and right-clicking a workspace (or the empty
rail) offers New, Rename, and Close Workspace plus **Bind to Host…** /
**Unbind from Host** for that slot — Rename edits the label in place, and Bind
routes the clicked workspace's new tabs to a chosen saved host (existing tabs
keep their shells). The terminal content menu carries the same New / Rename /
Close Workspace and Bind/Unbind actions (they act on the active workspace). Closing the
last tab in a workspace closes that workspace; closing the last workspace quits
the app. The rail follows the `workspace_rail` setting: `auto` (default) reveals it
once a second workspace exists, `always` pins it even with one, and `left` /
`right` pin it to that side. When more than one workspace exists, a tab's
right-click menu adds **Move to Next Workspace** to relocate the clicked tab.

**Restoring a layout.** With `restore_workspaces` on (off by default; the
Sessions section of Settings, or `ODYTTY_RESTORE_WORKSPACES`), launching
`odytty` with no arguments reopens the previous window shape — its workspaces,
tabs, and pane splits, each pane at its recorded working directory. Any
command-line argument suppresses restore, and only the primary instance
restores. The saved snapshot records **structure only** — never terminal
output, scrollback, environment, or the commands that were running — so a
restored pane is always a fresh shell at its directory, never a replayed
session. A workspace can also be saved as a **named layout** — from the command
palette (**Save Current Workspace as Layout**), a workspace rail slot's
right-click menu (**Save as Layout…**, saving the clicked workspace), or the
content-grid right-click menu (saving the active one). Opening a layout later —
**Open Layout** in the palette, or **Open Layout…** from the empty rail, the
empty tab strip, or the content-grid menu — appends it as a new workspace rather
than replacing the current one; with no layouts saved yet the picker explains
how to create one. On Unix, a restored or instantiated pane whose detached
session-host is still alive reattaches to it; a dead one silently opens a
fresh shell, with a compact "N of M sessions reattached" notice. Restore and
named layouts are cross-platform (the state dir uses `%LOCALAPPDATA%` on
Windows); session-host reattach is Unix-only, so a Windows restore always
lands fresh shells.

Any tab can be split into panes. The direct chords `Ctrl+Shift+E` (split into
columns, new pane on the right) and `Ctrl+Shift+O` (split into rows, new pane
below) create a split on a single-pane tab — they match Ghostty's Linux
defaults and work at both single-pane and multi-pane. You can also split from
the terminal's right-click menu's "Split Right" / "Split Down" items. When the
active tab is already multi-pane, that same content menu also offers a "Close
Pane" item (labelled with the effective `Ctrl+b x` prefix chord) to close just
the focused pane; it is hidden in a single-pane tab, where closing the tab is
the only close. The content menu also has a launcher section at the bottom —
"Connection Manager", "Command Palette", and "Session Replay" — each labelled
with its effective chord and opening the matching overlay. Right-clicking a tab
opens a separate, tab-scoped menu (New Tab, Rename Tab, Close Tab, Close Other
Tabs, **Connect to Host…**, **Replace with Host…**, New Window — plus Move to
Next Workspace when more than one workspace exists), and right-clicking the empty
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
discoverable menu entry — the launcher actions appear in the right-click menu's
launcher section, and the theme builder is an "Open Theme Builder" entry in the
Settings → Themes section. These chords are all `Ctrl+Shift+<letter>`, which a
TUI cannot receive, so PTY input is unchanged. Prompt navigation is the
`Ctrl+Shift+Up/Down` arrows. Rebind any of them, for example:

```conf
# odytty.conf
keybinds = ctrl+alt+p=command-palette
```

#### Shell Integration

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
will not send blind edit bytes — it clears the stale selection and surfaces the
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
`ODYTTY_SESSION_REPLAY=on` as a one-off override) — off by default, so the plain
path is unchanged — then rebind the scrub overlay if desired:

```conf
# odytty.conf
session_replay = on
keybinds = ctrl+alt+r=session-replay
```

For a one-off/dev override, run
`ODYTTY_SESSION_REPLAY=on ODYTTY_KEYBINDS="ctrl+alt+r=session-replay" odytty`;
env wins for that session.

The ring is capped (600 frames and 24 MiB, whichever binds first) and
local-only — frames never touch disk or the network. The overlay is
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
is validated — embedded spaces, a leading `-`, or an out-of-range port are
rejected so nothing option-injects into the `ssh` command line.

**Add / Edit form.** For more than a quick save, the connection manager opens an
in-app form: **Tab** starts a blank **Add connection** and the right arrow opens
an **Edit** pre-filled from the selected OdyTTY-owned host (`ssh-config`-imported
rows are read-only). Alongside alias / host / user / port, an **Advanced** section
carries an `IdentityFile` path (adds `ssh -i` on connect; a path, never a stored
secret — `ssh-copy-id` is the once-and-done way off passwords), the three-way
`Integration` / `Reuse` / `Tmux` overrides (**inherit / on / off**), and
theme / font / title. Saving appends a new block or edits the existing one in
place, leaving every other block, comment, and unknown field byte-for-byte
untouched. A **Test connection** button runs a non-interactive background probe
and reports an honest tri-state result — reachable with key/agent auth, reachable
but interactive-auth (expected for a password host; the connect still works), a
host-key mismatch, or unreachable — without ever handling a password.

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
a shared ControlMaster connection so they connect with no new handshake;
`remote_tmux` (off by default) wraps the remote shell in a persistent
`tmux new-session -A -s odytty` so a dropped-and-reconnected link resumes the
same remote session. When a remote connection drops, the tab is held open with
a reconnect prompt — **Enter** reconnects in place, **Esc** or **Ctrl+D**
closes the tab. Pasting a clipboard image into an integrated remote tab offers
a confirm-first upload (`remote_image_paste`, `ask` by default): on confirm the
image is streamed to a private temp file on the remote and its path is pasted
as text — nothing runs remotely. Each knob has a per-host override in
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

The `keybinds` config key can rebind local actions: `search`, `settings`,
`theme-picker`, `theme-builder`, `copy`, `paste`, `scroll-up`, `scroll-down`,
`jump-prompt-prev`, `jump-prompt-next`, `copy-mode`, `hints`, `clear-input`,
`command-palette`, `session-replay`, `connection-manager`, `session-attach`,
`new-tab`, `next-tab`, `prev-tab`, and `close-tab`. The pane actions
(`split-columns`, `split-rows`, `focus-pane-left` / `-right` / `-up` / `-down`,
`focus-pane-next`, `close-pane`, `zoom-pane`, `equalize-panes`) are rebindable
too — the chord is the key pressed *after* the prefix, e.g.
`keybinds = ctrl+f=zoom-pane`. `ODYTTY_KEYBINDS` provides the same syntax as a
session-scoped override. See [`docs/keybindings.md`](docs/keybindings.md) for the
complete keyboard reference — every default chord, the pane prefix, copy mode,
hints, and rebinding.

### Settings And Themes

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

#### Default background image (and how to turn it off)

Since v0.6.0 OdyTTY ships with its OdysseyOS visual identity on by default: the
default theme is `odyssey-default` (a deep forest-green palette; also reachable
under the `odyssey-jungle` alias), and an original "Dark Waves" background image
is **bundled into the binary and shown by default** behind the grid. The image
is embedded at build time, so it works identically on every install (source
build, AppImage, and distro package) with no external file to manage. It carries
the repository license (see [`assets/backgrounds/LICENSE`](assets/backgrounds/LICENSE)).

To turn the background **off**, set either key in `odytty.conf`:

```ini
# odytty.conf — disable the bundled background entirely
background_treatment = color   # draw the theme background only (no image)
# — or —
background_image = none        # keep image treatment available but use no image
```

To use **your own** image instead, point `background_image` at a file:

```ini
# odytty.conf — use a custom background
background_treatment = image
background_image = /path/to/your/wallpaper.png   # png / jpeg / webp
background_image_scrim = 0.5                      # 0 = none, 1 = opaque scrim; `auto` = floor-safe
```

`background_image = default` (the unset value) selects the bundled image again.

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

## Architecture

The terminal core and visual layer are deliberately separate:

| Area | Path |
| --- | --- |
| PTY | `src/pty.rs` |
| Parser | `src/parser/` |
| Terminal model | `src/core/` |
| Render geometry | `src/grid.rs`, `src/render.rs`, `src/boxdraw.rs` |
| Text atlas and font resolution | `src/atlas/`, `src/text.rs`, `src/emoji/` |
| Graphics protocols | `src/graphics/`, `src/core/graphics_routing.rs` |
| Settings | `src/settings.rs`, `src/settings/` |
| Theme system | `src/theme/`, `src/theme_author.rs`, `src/palette_gen.rs` |
| Command palette | `src/fuzzy.rs`, `src/palette.rs`, `src/palette_catalog.rs`, `src/palette_sources.rs`, `src/native/palette_overlay.rs` |
| Native app and GPU | `src/native/` |

External crates do not own terminal semantics. `vte`, `portable-pty`, and
`crossterm` are not in the dependency tree.

## Testing

The repository carries unit, integration, fuzz-smoke, pixel-smoke, PTY-smoke,
GPU-composite, and CLI tests. The default suite is intended to be deterministic
and host-independent; PTY smoke and deep fuzz tiers are ignored by default.

```sh
cargo test
cargo fmt --check

# Parser/protocol deep tier when touching those paths:
ODYTTY_FUZZ_ITERS=40000 cargo test --test protocol_fuzz -- --ignored --nocapture

# Evidence-only performance harness:
cargo bench --bench perf
```

The library test suite alone runs in the thousands of cases and is exercised on
every CI run; the current counts live in [`DEVLOG.md`](DEVLOG.md), with the full
tree carrying additional integration and smoke suites. See
[`CONTRIBUTING.md`](CONTRIBUTING.md) for the commit gate.

## Status

**Works today:** real shells, multi-session tabs, splits/panes with a
configurable tmux-style prefix, scrollback, search, selection,
copy/paste, font/theme/settings overlays, theme builder, 100 themes, color
emoji, Kitty graphics, Sixel, Kitty keyboard protocol, SGR-pixel mouse,
clickable paths with an inline image viewer, detached and in-window managed
sessions, a connection manager, command palette, session replay,
OSC 8/52/133, dynamic colors, prompt navigation, command status gutter,
readability and accessibility settings, bloom/CRT/retro effects, background
treatments, and a large compatibility test surface.

**Platforms:** Linux is the primary, battle-tested target. macOS is an
experimental build-from-source target (see Install And Run). Windows is
supported through ConPTY and ships as an unsigned portable zip. Linux, macOS,
and Windows are all blocking CI legs.

**Known gaps:** Windows detached/resumable session hosting, profiles, per-pane
inline graphics, Kitty animation, Kitty Unicode placeholders, iTerm2 graphics,
COLR/CPAL color fonts, and broader ligature/stylistic-set shaping.

The running history lives in [`DEVLOG.md`](DEVLOG.md). The current public
roadmap lives in [`TODO.md`](TODO.md) and
[`docs/full-build-roadmap.md`](docs/full-build-roadmap.md).

## Public Repository Safety

This repository is public. Do not commit secrets, credentials, API keys, tokens,
private hostnames or URLs, personal data, `.env` files, local-only config, or
machine-specific notes. Before any commit or push, inspect staged changes for
sensitive content.

OdyTTY itself is local-first: no telemetry, no account, no cloud sync, no
analytics, no crash reporting, and no update pings.

## Project Docs

- [`SPEC.md`](SPEC.md) — product charter and architecture decisions.
- [`TODO.md`](TODO.md) — current milestone checklist and remaining work.
- [`DEVLOG.md`](DEVLOG.md) — reverse-chronological development record.
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — project stance on contributions, plus
  testing and public-repo safety rules.
- [`SECURITY.md`](SECURITY.md) — supported versions and private vulnerability
  reporting.
- [`PACKAGING.md`](PACKAGING.md) — downstream package install surface and
  release packaging notes.
- [`docs/install.md`](docs/install.md) — source builds, desktop launcher
  registration, AppStream metadata, Odyssey/LFS packaging, and default-terminal
  notes.
- [`docs/release.md`](docs/release.md) — release artifact checklist and
  Odyssey-Mon upstream tracking notes.
- [`docs/runtime-knobs.md`](docs/runtime-knobs.md) — settings reference.
- [`docs/keybindings.md`](docs/keybindings.md) — complete keyboard reference and
  rebinding.
- [`docs/accessibility.md`](docs/accessibility.md) — contrast floor, color-vision
  modes, dimming, motion, and bell.
- [`docs/themes.md`](docs/themes.md) — theme format and built-in library.
- [`docs/graphics.md`](docs/graphics.md) — Kitty graphics and Sixel support.
- [`docs/visual-architecture.md`](docs/visual-architecture.md) — renderer and
  visual-layer architecture.
- [`docs/hidpi-validation.md`](docs/hidpi-validation.md) — manual HiDPI checks.
- [`docs/full-build-roadmap.md`](docs/full-build-roadmap.md) — long-range map.

## License

OdyTTY is licensed under **GPL-3.0-only**. See [`LICENSE`](LICENSE).

You may use, study, share, and modify the source under that license. If you
distribute a modified version, you must release your changes under the same
license.

Copyright (C) 2026 Unfinished Works and the OdyTTY contributors.

The OdyTTY name and branding are separate from the source license. Forks and
modified builds should use their own name and must not imply endorsement by
Unfinished Works. See [`NOTICE`](NOTICE).

Contributions are accepted under the Developer Certificate of Origin. See
[`CONTRIBUTING.md`](CONTRIBUTING.md).
