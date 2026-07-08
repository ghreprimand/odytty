# Diagnostics, Logging, and Crash Reporting

OdyTTY keeps a small, local diagnostics trail so a misbehaving session can be
investigated after the fact — without ever recording what you typed or what your
programs printed. Everything here is a local file or stderr: there is no
telemetry, no crash-reporting service, no network egress of any kind. That is a
deliberate, permanent project stance, not a default you can accidentally flip.

This document covers where the logs live on each platform, how large they can
grow, what is and is not captured, and the opt-in trace gates.

## The privacy floor (read this first)

No terminal content ever reaches any log or crash file. Concretely, none of
these are written anywhere:

- PTY bytes (program output),
- scrollback,
- keystrokes or typed input,
- window/tab titles,
- the working directory or file paths from your session.

The diagnostics sinks record only *program state*: panic metadata, a freeze
watchdog's latch snapshot, GPU adapter identity (hardware metadata), and
bounded application log lines. The privacy boundary is enforced by construction
and pinned by tests — the watchdog's state record, for example, is built from
booleans and counters that have no way to hold a string, so terminal text
cannot flow into it even by mistake.

One deliberate exception, stated for completeness: operating-system **error
strings** are logged as-is (for example, the message from a failed file open),
and such an OS message can occasionally embed a filesystem path. This is
OS-authored error text, not terminal content, and it is intentional — it is what
makes a logged failure actionable.

## Where the logs live

Two files live in a per-user state directory, namespaced to `odytty` on every
platform:

- `odytty.log` — the rotated application log (warnings and above by default).
- `odytty.log.1` — the single rotated predecessor of `odytty.log`.
- `panic.log` — written only when the process crashes to abort.

| Platform | Log directory | Source | Fallback |
|---|---|---|---|
| **Linux / BSD** | `$XDG_STATE_HOME/odytty` | `XDG_STATE_HOME` | `~/.local/state/odytty`, then the OS temp dir |
| **macOS** | `~/Library/Logs/odytty` | `HOME` | the OS temp dir |
| **Windows** | `%LOCALAPPDATA%\odytty` | `LOCALAPPDATA` | `%TEMP%\odytty` |

**Windows is first-class here.** The log directory resolves to
`%LOCALAPPDATA%\odytty`, which is persistent per-user storage. This is
deliberate: earlier Windows builds fell through to `%TEMP%`, which Windows
periodically cleans, so a log could vanish before it could be retrieved for a
support request. Windows also runs as a GUI-subsystem application with no
visible stderr, so the on-disk `odytty.log` is the primary way to see what the
build reported at startup. The path resolution is unit-tested on the Windows CI
leg.

The directory is created lazily, on the first actual log write — a quick
`odytty --version` or `--show-config` never touches it. If every candidate
directory is unavailable, resolution falls back to the OS temp directory rather
than failing, so logging can never take the terminal down.

## Size bounds

The logs are bounded by design; they will not grow without limit.

- **`odytty.log` is hard-capped at 2 MiB.** When a write would exceed the cap,
  the current file is rotated to `odytty.log.1` (replacing any prior `.1`) and a
  fresh `odytty.log` is started. With one predecessor kept, on-disk usage is
  bounded at roughly **4 MiB total**. An oversized file left by an earlier run
  rotates before the first new append.
- **`panic.log` is intentionally uncapped.** It is written only on an actual
  crash-to-abort, and the process exits immediately after a single record (one
  metadata line plus a backtrace), so it does not grow during normal use. This
  is a deliberate choice, not an oversight: a crash record must never be dropped
  or truncated to satisfy a rotation cap.

Logging is also infallible: every I/O error while writing or rotating is
swallowed, so a full disk or an unwritable directory degrades to "no log," never
to a crash.

## What is captured

- **Panic hook.** Installed before the event loop starts, process-wide. On a
  panic it records the panic message, source location (`file:line:column`), the
  thread name, and a forced backtrace, then aborts the process. Writing three
  independent sinks — stderr, `odytty.log`, and a structured `panic.log` line —
  each guarded so the report is still emitted even if one sink fails. The
  process aborts rather than exiting so a panicking render or worker thread dies
  visibly instead of stranding a frozen, zero-CPU window, and so a debugger or
  coredump still sees the crash site.
- **Freeze watchdog.** A lightweight monitor thread watches for the signature of
  a hang: input or redraw work pending with no frame presented for about ten
  seconds. When it fires it emits a single log record naming its internal latch
  state — no dump, no signal, no process action — so the next freeze is
  diagnosable from the log rather than requiring a live debugger. It is
  rate-limited to at most one record per minute per stall, costs a few atomic
  stores per event on the healthy path, and re-arms as soon as a frame is
  presented.
- **GPU adapter identity.** At startup OdyTTY records the selected GPU adapter's
  name, backend, device class, and driver — hardware metadata only, no user
  content. If the selected adapter is a software rasterizer (llvmpipe, lavapipe,
  SwiftShader, or WARP), it logs a prominent software-rendering warning pointing
  at the "Slow rendering / software adapter" section of
  [`install.md`](install.md). These startup lines go to `odytty.log` as well as
  stderr, so the adapter identity and the software-render warning survive even
  when a launcher discards stderr — and are retrievable on Windows, where there
  is no visible stderr at all. The same adapter details are shown live in the
  About panel.
- **Application log.** Warnings and above from across the app, teed to stderr and
  the rotated `odytty.log`.

## Log level and trace gates

### `RUST_LOG` — runtime log level

`RUST_LOG` sets the log level. It accepts a **bare level token** only —
`error`, `warn`, `info`, `debug`, or `trace` (any case). The default is `warn`.

```sh
RUST_LOG=info odytty
```

This is a bare-level parser, **not** the full `tracing_subscriber` per-target
directive syntax. Per-target filters such as `RUST_LOG=odytty::reflow=warn` are
**not** supported — an unparseable value falls back to `warn` rather than
silencing anything (a typo must never hide errors).

### Opt-in traces

One targeted trace gate exists. It is inert unless explicitly set and is meant
for reproducing a specific issue, not for everyday use.

| Gate | Value | Destination | Purpose |
|---|---|---|---|
| `ODYTTY_REFLOW_TRACE` | `1` or `true` | `odytty-reflow-trace.log` in the OS temp dir | Permanent passive diagnostic: one geometry/cursor line per terminal resize. |

`ODYTTY_REFLOW_TRACE` costs a single atomic load when off and appends one line
per resize when on; it records geometry and cursor coordinates only, never cell
contents, paths, or environment values. It writes to the OS temp directory
because the Windows GUI build has no visible stderr, so the file is retrieved
afterward.

## Retrieving logs for a support request

Send `odytty.log` (and `odytty.log.1` if present) from the log directory for
your platform above. On Windows that is `%LOCALAPPDATA%\odytty`. These files
contain only the bounded, privacy-preserving diagnostics described here — no
terminal content — so they are safe to share.
