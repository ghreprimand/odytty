# Security Policy

OdyTTY is a terminal emulator: it parses and renders **untrusted byte streams** —
program output, escape sequences, clipboard and OSC payloads, and file paths
lifted from directory listings. The parser and the input / clipboard / file-open
paths are therefore a genuine security surface, and security reports are taken
seriously even though this is a small, personal, maintainer-led project.

## Supported versions

OdyTTY ships as a rolling release with source and prebuilt artifacts. Only the
**latest tagged release** (and `master`) receive security fixes; there are no
long-term support branches. Check your version with `odytty --version`.

## Reporting a vulnerability

**Please do not open a public issue, pull request, or discussion for a security
vulnerability.** Public disclosure before a fix exists puts users at risk.

Report it privately through GitHub's private vulnerability reporting:

1. Open the repository's **Security** tab → **Report a vulnerability** (GitHub
   Security Advisories).
2. Include the affected version (`odytty --version`) and the smallest
   reproduction you can — ideally the exact byte sequence, config, or steps that
   trigger the issue.

This opens a private advisory visible only to you and the maintainer; no email
address or other personal contact is required.

## What to expect

- This is a best-effort, solo-maintained project: acknowledgement and fixes are
  handled as promptly as is reasonable, but there is no guaranteed response time.
- Valid reports are addressed with a coordinated fix and release. Please allow a
  reasonable window before any public disclosure.
- Reporters who want credit will be credited.

## In scope

- Memory-safety or panic-to-crash bugs reachable from untrusted terminal output
  or escape-sequence parsing (`src/parser/`, `src/core/`).
- Escape sequences that can exfiltrate data, write or read outside intended
  bounds, or open/execute something without user intent (OSC handlers, OSC 52
  clipboard, the interactive file-open paths).
- Detached-session host/socket issues that cross a trust boundary
  (`src/session_host/`).
- Workspace/layout snapshot leakage: the persistence snapshot is defined to
  record structure only — workspace and tab names, pane split ratios, and each
  pane's working directory — and to NEVER capture terminal grid content,
  scrollback, environment, or the commands that were running. A restored local
  pane opens a fresh shell at the captured directory and never re-executes a
  captured command. On Unix, a pane whose detached session host is still alive
  reattaches to that running session and its scrollback. A captured SSH pane
  reconnects to a fresh remote login shell at the remote's default directory.
  A snapshot that persists any of the excluded content is in scope.
- Remote image paste-through writing outside its intended bounds: an image
  pasted into a remote session is uploaded over the existing SSH connection when
  reuse is available on Unix. With reuse off, and on Windows, the upload opens a
  separate SSH connection. It writes a temporary file created `0600` under
  `umask 077`, and its path is copied to
  the clipboard rather than injected into the shell for execution. A path that
  escapes the temp location, world-readable upload permissions, or the path being
  auto-executed is in scope.

## Out of scope

- Issues that require the user to deliberately enable an explicitly-documented
  unsafe option, or to run an obviously hostile command themselves.
- Visual or rendering glitches with no safety impact — file those as normal
  issues (subject to the contribution policy in `CONTRIBUTING.md`).
