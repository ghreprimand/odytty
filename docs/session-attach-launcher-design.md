# Session Attach Launcher Design

This record covers the detached-session attach flow after v0.3.1 and the Phase
5 in-window launcher follow-up.

## Principle: Summon, Not Greet

Session attach is a tool the operator summons when needed. OdyTTY should not
open a pre-window chooser or block normal startup behind a session picker. A
normal launch still opens the normal terminal path; attach UI appears only when
the operator asks for it through `odytty attach`, a keybinding, or a menu item.

## CLI Attach Resolution

`odytty attach <id>` keeps its existing meaning: open a live native window, keep
the normal local shell, and attach the requested hosted session as a focused tab.

`odytty attach` with no id resolves against the live local session registry:

- No live sessions: exit with a clear `no live sessions to attach` message.
- One live session: attach that session.
- Several live sessions: print the session list and require an explicit choice:
  `odytty attach <id>`.

The multiple-session case must never auto-attach. Requiring an id avoids
surprising the operator when more than one resumable session exists.

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

Phase 5 adds a `session-attach` overlay inside an already-open OdyTTY window.
That overlay is a summoned in-window control, not a startup greeter. It should
reuse the command/connection overlay pattern: take a frozen session-list
snapshot, type to filter, navigate with arrows, dismiss with `Esc`, and attach
with `Enter`.

Accepting a row attaches the selected hosted session into a new tab. The existing
tab and pane layout remain intact.

### Status: shipped (Phase 5 / B2)

The overlay shipped as `OverlayMode::SessionAttach`, a structural clone of the
connection-manager overlay sourced from
`session_host::list_live_sessions(None)`. It is **presentation-only**: a frozen
list captured at open time, type-to-filter fuzzy matching over title and id,
`↑`/`↓` selection, `Enter` → attach into a **new tab** via
`App::attach_session_in_new_tab`, `Esc` → dismiss. An empty live set opens to a
"No live sessions" hint rather than failing. Session names are control-char
sanitized before display (the `--title` is user-supplied). A session that ended
between listing and accept yields an `Err` from the attach path, which the App
swallows like the connection-manager connect arm — no panic.

Summoning paths:

- **Chord:** the bindable `session-attach` action, default `Ctrl+Shift+A` (a
  TUI-unreachable `Ctrl+Shift+<letter>` chord, consistent with the v0.3.1
  discoverability family; verified free against the existing letter set).
- **Right-click menu:** an "Attach Session" item in the launcher section of the
  context menu (alongside Connection Manager / Command Palette / Session Replay),
  whose accelerator label auto-tracks the bound chord via the existing
  `set_accelerators` path.

Closed overlay = live frame byte-identical (the mode is never entered until
summoned); `gpu_composite_smoke` stays 3/3.

## Rejected Option 4: Pre-Window Launcher

The pre-window launcher option is rejected. A chooser before the native window
would make startup conditional on external session state, create a second launch
surface to theme and test, and blur the distinction between ordinary terminal
startup and explicit attach workflows. It also conflicts with the "summon, not
greet" rule above.

Session selection belongs either in the explicit `odytty attach` CLI path or in
the Phase 5 in-window overlay.
