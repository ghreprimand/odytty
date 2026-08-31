# Notifications, Progress, And Pane Monitors

OdyTTY treats terminal-authored notifications and progress as untrusted,
transient hints. They never replace terminal grid content, send input, open a
URL, create an action, or persist in a workspace snapshot.

## Supported OSC Conventions

| Sequence | Supported form | Result |
| --- | --- | --- |
| OSC 9 | `OSC 9 ; message ST` | Bounded notification request |
| OSC 777 | `OSC 777 ; notify ; title ; body ST` | Bounded notification request |
| OSC 9;4 | `OSC 9 ; 4 ; 0 ST` | Clear progress |
| OSC 9;4 | state `1`, `2`, or `4` plus `0..100` | Normal, error, or paused determinate progress |
| OSC 9;4 | state `3` with no value | Indeterminate progress |

BEL and ST terminators are accepted through the shared bounded OSC parser.
Other OSC 9;4 states, extra fields, invalid numbers, values above 100, unknown
OSC 777 verbs, and malformed requests are ignored. A notification payload is
limited to 1,024 wire bytes. At most eight requests await presentation between
drains.

Control characters are stripped during parsing. Terminal-authored titles and
bodies are not displayed in trusted OdyTTY chrome or desktop notifications;
presentation uses generic application-owned wording. This prevents output from
placing credentials, private paths, URLs, markup, or command-like text in a
trusted notification surface.

## Presentation And Ownership

Progress and notification state belongs to the exact pane that emitted it.
Tabs and workspaces show only a bounded rollup of their owned panes. Viewing an
active tab clears unread/completion flags while leaving live progress intact.
In-app notices expire after 15 seconds; progress expires after 10 minutes if a
program never clears it. Per-pane duplicate requests are suppressed for five
seconds and at most four requests are accepted in 30 seconds. A second
application-level limiter bounds native delivery attempts.

`notifications` and `ODYTTY_NOTIFICATIONS` select presentation:

| Value | Behavior |
| --- | --- |
| `off` | Drain and discard notification/progress presentation; pane monitor actions are unavailable |
| `in-app` | Transient pane-owned notices, badges, and progress only; this is the default |
| `attention` | In-app state plus an unfocused window-manager attention request |
| `desktop` | In-app state plus an on-demand desktop notification attempt |

BEL remains independently controlled by `bell`. No notification path steals
keyboard focus.

The command palette exposes **Notify When This Command Finishes** only for a
currently running, verified OSC 133 command. The authorization is one-shot and
is consumed by the next explicit OSC 133 `D` boundary. The palette also exposes
one-shot monitors for pane activity, 30 seconds of silence, BEL, process exit,
and an explicit nonzero OSC 133 command status, plus **Clear Pane Monitors**.
These monitor flags are transient and are not restored.

Named launch profiles ship in v0.14.0. v0.13.0 therefore provides the global
setting through the established defaults/config/environment resolver and keeps
the transient pane state separate so the later profile override can use the
same policy without a second notification model.

## Platform Adapters

No notification discovery runs during startup. Delivery is attempted only
after policy and rate-limit checks:

| Platform | Adapter | Failure behavior |
| --- | --- | --- |
| Linux Wayland | On-demand freedesktop `notify-send` helper | In-app state remains authoritative |
| Linux X11 | On-demand freedesktop `notify-send` helper | In-app state remains authoritative |
| macOS | On-demand `osascript` notification request with fixed script and argv text | Permission or adapter failure leaves in-app state |
| Windows | On-demand PowerShell Windows toast request with fixed application-owned XML | Permission or policy failure leaves in-app state |

Automated tests pin each command specification independently without claiming
that desktop hardware or a notification service accepted delivery. Manual
platform acceptance remains required for a release claim.
