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
Tabs and workspaces show only a bounded rollup of their owned panes: a tab's
progress glyph sits in its top-strip padding, and a workspace's sits at the
workspace rail's activity edge, where progress takes the place of the unseen
activity dot. Viewing an
active tab clears unread/completion flags while leaving live progress intact.
A later activity, silence, BEL or process-finish notice preserves an unseen
command-failure badge until that pane is viewed or its attention is cleared.
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

The notification setting is global: it resolves through the established
defaults/config/environment resolver, and named launch profiles carry no
notification override. The transient pane state is kept separate from the
setting.

## Platform Adapters

No notification discovery runs during startup. Delivery is attempted only
after policy and rate-limit checks:

| Platform | Adapter | Failure behavior |
| --- | --- | --- |
| Linux Wayland | On-demand freedesktop `notify-send` helper | In-app state remains authoritative |
| Linux X11 | On-demand freedesktop `notify-send` helper | In-app state remains authoritative |
| macOS | On-demand `osascript` notification request with fixed script and argv text | Permission or adapter failure leaves in-app state |
| Windows | On-demand PowerShell Windows toast request with fixed per-kind application-owned XML and explicit WinRT type loading | Permission or policy failure leaves in-app state |

Automated tests pin each command specification independently without claiming
that desktop hardware or a notification service accepted delivery. Release
evidence records on-device acceptance as a separate result and never infers it
from command construction or hosted CI.

Each notification helper gets ten seconds to finish. On timeout it is killed
and reaped. Its input and output streams are closed; unsuccessful exits and
helper failures produce a generic debug diagnostic without terminal text.
This bound applies on Linux Wayland, Linux X11, macOS and Windows. Windows
requests use distinct fixed wording for terminal attention, command completion,
command failure and pane monitors. Delivery still depends on the desktop
service, application registration and notification policy; in-app state
remains authoritative.
