# Session Attach Launcher Design

This record covers the Unix-only detached-session attach flow and its in-window
launcher. On Windows, Manage Sessions opens an empty overlay and attach requests
return a clean unsupported-platform error.

## Principle: Summon, Not Greet

Session attach is a tool summoned when needed. OdyTTY should not
open a pre-window chooser or block normal startup behind a session picker. A
normal launch still opens the normal terminal path; attach UI appears only when
requested through `odytty attach`, a keybinding, or a menu item.

## CLI Attach Resolution

`odytty attach <id>` keeps its existing meaning: open a live native window, keep
the normal local shell, and attach the requested hosted session as a focused tab.

`odytty attach` with no id resolves against the live local session registry:

- No live sessions: exit with a clear `no live sessions to attach` message.
- One live session: attach that session.
- Several live sessions: print the session list and require an explicit choice:
  `odytty attach <id>`.

The multiple-session case must never auto-attach. Requiring an id avoids
surprising behavior when more than one resumable session exists.

`odytty attach --diagnostic <id>` remains the script/CI form. It prints the
diagnostic status row and exits without opening a window.

## List Row Schema

Human-facing session lists show the readable name first, then enough metadata to
choose deliberately:

```text
build    1 pane     42s     (s-0001-aaaa)
web      2 panes    5m      (s-0002-bbbb)
```

If the session name is the id, the row omits the duplicate parenthesized id:

```text
s-0003-cccc    1 pane    8s
```

Names come from detached-session metadata. `odytty new --detached --title build`
therefore lists as `build`, while sessions without a title fall back to their id.
The id stays visible whenever the name differs because `odytty attach <id>` needs
an exact target.

## In-Window Overlay Rule

A `session-attach` overlay lives inside an already-open OdyTTY window. That
overlay is a summoned in-window control, not a startup greeter. It reuses the
command/connection overlay pattern: take a frozen session-list snapshot, type to
filter, navigate with arrows, dismiss with `Esc`, and accept with `Enter`.

Accepting a row does not unconditionally open a new tab. The accepted session id
is routed through dedup and a choice dialog (see below): an already-open session
switches to its existing tab, while a fresh session prompts New tab vs Replace.
The Replace path closes the current tab; the New-tab path keeps the existing tab
and pane layout intact.

### Status: shipped on Unix

The overlay is `OverlayMode::SessionAttach`, a structural clone of the
connection-manager overlay sourced from
`session_host::list_live_sessions(None)`. The list is **presentation-only**: a
frozen snapshot captured at open time, type-to-filter fuzzy matching over title
and id, `↑`/`↓` selection, `Esc` → dismiss. An empty live set opens to a
"No live sessions" hint rather than failing. Session names are control-char
sanitized before display (the `--title` is user-supplied). A session that ended
between listing and accept yields an `Err` from the attach path, which the App
swallows like the connection-manager connect arm — no panic.

`Enter` emits an attach outcome carrying the selected session id, which the App
routes through `route_attach_session`:

- **Already-open session:** dedup switches focus to that session's existing tab
  rather than attaching a second copy.
- **New session:** a New-tab vs Replace choice dialog (`AttachChoice`) opens.
  `[N]`/`Enter` attaches into a **new tab** (`attach_session_in_new_tab`); `[R]`
  attaches then **closes the current tab** (`attach_session_replacing_current`).

#### Kill a session from the overlay

A managed session can also be terminated from the overlay: right-clicking a row
requests a kill, which opens a `ConfirmKillSession` dialog. On confirm the App
calls `session_host::kill_session` and reopens the manager so the list reflects
the change. A socket that is already gone is treated as `Ok`, so a stale row
kills cleanly without error.

#### Tab rename scope

Renaming an attached tab renames the **GUI tab label only**. It does not write
back to the session-host sidecar metadata, so the next listing of that session
(in the overlay or via `odytty list`) still shows its original `--title`. The
overlay does not persist renames.

Summoning paths:

- **Chord:** the bindable `session-attach` action, default `Ctrl+Shift+A`. A
  `Ctrl+Shift+<letter>` chord is used because a TUI running inside the terminal
  cannot receive it, so the launcher stays reachable without colliding with
  in-shell input. Rebindable like every other action — see
  [`keybindings.md`](keybindings.md).
- **Right-click menu:** a "Manage Sessions" item in the launcher section of the
  context menu (after Command Palette / Session Replay), whose accelerator label
  auto-tracks the bound chord via the existing `set_accelerators` path.

Closed overlay = live frame byte-identical (the mode is never entered until
summoned); the GPU composite smoke suite guards that invariant.

### Related: Detach & switch (Unix only)

The context menu also exposes a **Detach & switch** action, which shares this
attach plumbing: it spawns a fresh managed session in the focused pane's working
directory and attaches it (an honest spawn, not a live migration of the running
process). It is a sibling of the manage-sessions surface described here rather
than part of the attach overlay itself.

## Rejected: Pre-Window Launcher

The pre-window launcher option is rejected. A chooser before the native window
would make startup conditional on external session state, create a second launch
surface to theme and test, and blur the distinction between ordinary terminal
startup and explicit attach workflows. It also conflicts with the "summon, not
greet" rule above.

Session selection belongs either in the explicit `odytty attach` CLI path or in
the in-window overlay.
