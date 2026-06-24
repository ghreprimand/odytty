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

## Rejected Option 4: Pre-Window Launcher

The pre-window launcher option is rejected. A chooser before the native window
would make startup conditional on external session state, create a second launch
surface to theme and test, and blur the distinction between ordinary terminal
startup and explicit attach workflows. It also conflicts with the "summon, not
greet" rule above.

Session selection belongs either in the explicit `odytty attach` CLI path or in
the Phase 5 in-window overlay.
