# Buttons — program-defined clickable output

OdyTTY lets a program mark a run of its own output as a clickable button.
Clicking the button sends a small, terminal-composed escape sequence back to
the program — an integer code, nothing else — so command-line tools can offer
"click to retry", "click to copy", or "click to open" affordances without a
GUI. Buttons survive scrollback, degrade to plain text in other terminals, and
are **off by default**.

This document is the wire-protocol reference for emitters. For the
shell-integration helper functions see [Helpers](#helpers); for a working
example run `scripts/button-demo.sh`.

## Design guarantees

- **Default-off, byte-identical.** With the `buttons` master gate off (the
  default), OdyTTY parses and discards the sequences below. Nothing is stored,
  nothing renders differently, and clicks never produce a report. The gate is
  enforced independently at the parser and at the pointer, so turning it off
  also kills clickability of anything already on screen.
- **The terminal owns the report.** The click report is composed by OdyTTY
  from the parsed integer code alone. A program cannot supply report bytes:
  the reply alphabet is exactly `ESC [ ? 0-9 ; ~` — no newline, no carriage
  return, no byte a shell or line editor treats as "execute". A hostile
  emitter chooses *which number* arrives, never *what byte shape* arrives.
- **Graceful degradation.** The native spelling brackets an ordinary text
  label between two private OSC sequences. Terminals that do not implement the
  protocol drop the unknown OSCs and print the label as plain text.

## Tier 2 — native spelling (preferred)

Define a button by bracketing its label:

```
OSC 133 ; P ; odytty-button ; code=N [; icon=NAME] [; scope=block|sticky] ST
  ...label text, printed as ordinary cells...
OSC 133 ; P ; odytty-button ; end ST
```

`ST` is either the two-byte string terminator (`ESC \`) or `BEL`. Fields:

| field | required | meaning |
|---|---|---|
| `code=N` | yes | ASCII decimal, `1..=4294967295` (at most 10 digits). `0`, empty, and overflowing values reject the definition. |
| `icon=NAME` | no | Semantic icon name (see [Icons](#icons)). Unknown names fall back to a generic glyph. |
| `scope=block\|sticky` | no | Lifetime (see [Lifetime](#lifetime)). Default `block`. |

Duplicate fields, unknown fields, and unknown scope values reject the whole
definition; the label then prints as plain text. The signal name is versioned:
a future `odytty-button2` revision will be ignored by today's parser rather
than misparsed.

Invalidate buttons (they gray out and stop reporting, but keep their cells):

```
OSC 133 ; P ; odytty-button ; invalidate ST            # all buttons
OSC 133 ; P ; odytty-button ; invalidate ; code=N ST   # every button with code N
```

Example (printf spelling, bash/zsh):

```sh
printf '\e]133;P;odytty-button;code=42;icon=retry\a[ Retry ]\e]133;P;odytty-button;end\a\n'
```

## Tier 1 — iTerm2-compatible spelling

For programs that already emit iTerm2 custom buttons:

```
OSC 1337 ; Button=type=custom ; code=N [; icon=NAME] ST
```

This anchors a **point button** at the cursor: there is no label run, and
OdyTTY renders it as a chip at the end of the line's content. `icon=` accepts
the SF Symbols identifiers iTerm2 documents (they map onto the semantic icon
set below). A bare `OSC 1337 ; Button=type=custom ST` with no code invalidates
all buttons, matching iTerm2. `type=copy` and other `Button=` variants are
recognized and ignored. Tier 1 buttons take the `sticky` lifetime, matching
their iTerm2 semantics.

This spelling is gated separately (`buttons_iterm_compat`, on by default when
the master gate is on).

## The click report

Clicking a live button writes this to the program's input:

```
CSI ? 1337 ; code ~
```

For `code=42` the exact bytes are `1b 5b 3f 31 33 33 37 3b 34 32 7e` — the
same report iTerm2 sends for its custom buttons, so existing readers work
unchanged.

Interaction rules:

- A click is a **press and release on the same span**. Dragging off the
  button, scrolling, or losing focus between press and release cancels it.
- A plain left click activates. The click that focuses the window does not.
- A full-screen application that has mouse reporting enabled wins: its mouse
  protocol receives the click and no button report is sent.
- While a shell prompt is active, `sticky` buttons do not report (a stray
  click on old output should not type into a live prompt). `block` buttons
  still report, since a live block button at a prompt is part of that
  prompt's own output.
- Invalidated buttons render dimmed and never report.

## Lifetime

| scope | lives until |
|---|---|
| `block` (default) | the next shell-integration prompt boundary (OSC 133 `A`/`D`). Most buttons are meaningless once their program has exited. |
| `sticky` | explicitly invalidated, or the last line referencing it leaves scrollback. |

Sticky buttons keep working from scrollback: a `[ Retry ]` printed 500 lines
ago still reports when clicked. The `buttons_sticky` sub-gate (on by default)
downgrades `sticky` definitions to `block` when disabled.

Bounds: at most 16 button spans per line and 8192 distinct buttons overall.
At the ceiling, **new definitions are refused** — OdyTTY never evicts a button
that is still visible, because a visibly dead button is worse than a refused
new one. Definitions on the alternate screen are refused (full-screen
applications have real mouse protocols already).

## Icons

Icon names are semantic and platform-neutral. Accepted names and aliases
(case-insensitive; the aliases cover iTerm2's SF Symbols identifiers):

| icon | names |
|---|---|
| run | `run`, `play`, `play.fill` |
| retry | `retry`, `arrow.clockwise`, `repeat` |
| copy | `copy`, `doc.on.doc`, `doc.on.clipboard` |
| open | `open`, `folder`, `arrow.up.right.square` |
| stop | `stop`, `stop.fill`, `xmark` |
| check | `check`, `checkmark`, `checkmark.circle` |
| star | `star`, `star.fill` |
| info | `info`, `info.circle` |
| warn | `warn`, `warning`, `exclamationmark.triangle` |

Anything else renders the generic button glyph.

## Helpers

The shell-integration snippets (see `odytty shell-integration <shell>`) define
emitter helpers so scripts do not need to hand-roll escape sequences.

bash / zsh / fish:

```sh
odytty_button CODE LABEL [ICON] [SCOPE]   # print a clickable label
odytty_button_clear [CODE]                # invalidate all buttons, or one code
```

```sh
odytty_button 42 '[ Retry ]' retry
odytty_button 7 '[ Copy path ]' copy sticky
odytty_button_clear 42
```

PowerShell:

```powershell
Write-OdyttyButton -Code 42 -Label '[ Retry ]' -Icon retry
Write-OdyttyButton -Code 7 -Label '[ Copy path ]' -Icon copy -Scope sticky
Clear-OdyttyButton -Code 42   # or bare Clear-OdyttyButton for all
```

The helpers validate the code (positive integer) and emit nothing on misuse,
so a bad invocation can never leave a half-open bracketed run. They also guard
on the `ODYTTY_BUTTONS` discovery variable (below): without it, the define
helper prints the bare label and the clear helper is a no-op, so scripts can
call them unconditionally in any terminal.

## Settings

Buttons are configured by three settings (config file, environment variable,
or the settings panel, Input group). Changes apply live to every open session.

| setting | env | default | effect |
|---|---|---|---|
| `buttons` | `ODYTTY_BUTTONS` | off | Master gate: parse definitions, render chips, report clicks, and advertise support to new sessions. |
| `buttons_iterm_compat` | `ODYTTY_BUTTONS_ITERM_COMPAT` | off | Also accept the Tier 1 iTerm2 spelling. |
| `buttons_sticky` | `ODYTTY_BUTTONS_STICKY` | off | Honor `scope=sticky` lifetimes; off downgrades them to `block`. |

The sub-gates do nothing while the master gate is off.

## Windows

Fully supported. The sequences pass through ConPTY unmodified in both
directions: definitions parse identically, and the click report reaches the
program like any other terminal input. The PowerShell helpers above ride the
standard shell-integration injection, and the `ODYTTY_BUTTONS` discovery
variable is folded into the ConPTY environment block at spawn like any other
variable.

## Feature discovery

When the `buttons` master gate is on, OdyTTY sets `ODYTTY_BUTTONS=1` in the
environment of every new terminal session. Programs that want to tailor their
output can test it:

```sh
if [ -n "${ODYTTY_BUTTONS-}" ]; then
  # buttons are supported and enabled
fi
```

When the gate is off the variable is absent, so the test tracks the setting
exactly. Testing is optional: emitting the Tier 2 spelling unconditionally is
safe by design, since other terminals print the plain label and drop the
unknown sequences.

Known limitation: environment-based discovery does not cross ssh or nested
sessions (the same tradeoff as `TERM_PROGRAM` and similar variables), and a
session spawned before the setting was turned on keeps its old environment
until restarted. A query-escape mechanism may be added later if remote
discovery is needed.

## Current limitations

- Activation is pointer-only; keyboard activation and screen-reader
  affordances are planned but not yet designed.
- Button definitions are refused on the alternate screen (deliberate, see
  [Lifetime](#lifetime)).
