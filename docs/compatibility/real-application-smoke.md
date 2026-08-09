# Real-application smoke matrix

This matrix is the short interactive compatibility check for OdyTTY release
candidates. It complements automated parser, transcript, and conformance tests;
it does not replace the platform-wide checks in
[`manual-validation.md`](../manual-validation.md).

Run each platform table against the same clean release-candidate commit and the
fresh release-profile artifact recorded by that platform's manual-validation
record. Record `PASS`, `FAIL`, `SKIP`, or `UNSUPPORTED` using the vocabulary in
the manual checklist. An unavailable optional application is `SKIP`, not
`PASS`. Any application substitution must preserve the same category and be
named with its public version.

Use synthetic content only. Do not record shell history, real prompts, private
paths, hostnames, clipboard contents, or unredacted captures. The remote alias,
when needed, is `odytty-manual-host`.

## Shared observations

Every row exercises the following bounded observations where the application
supports them:

1. Launch the application from the platform's default OdyTTY shell and confirm
   the first frame, cursor, title, and input focus.
2. Enter synthetic ASCII, combining marks, wide CJK characters, emoji, box
   drawing, and right-to-left text. Exercise a native IME in the editor row and
   record `SKIP` with the missing input-method reason when unavailable.
3. Exercise application key handling, mouse reporting where supported, slow and
   rapid resize, scroll or search, and terminal selection plus clipboard copy
   and paste of synthetic text.
4. Exit normally, interrupt once where meaningful, and confirm that the shell
   prompt, cursor, modes, title, mouse behavior, and screen contents restore
   without a hang or stale alternate-screen frame.

One failed shared observation fails that row. Record a concise application-
specific reproduction rather than averaging several observations into a pass.

## Linux

| ID | Category | Application | Bounded workload and expected result | Result | Evidence |
| --- | --- | --- | --- | --- | --- |
| RA-LINUX-SHELL | Default shell | The host's configured default shell; record name and public version | Run synthetic Unicode output, a failing command, a pipeline, foreground interruption, and a subshell. Input, status, signals, resize, clipboard, and final `exit` remain correct. |  |  |
| RA-LINUX-EDITOR | Editor | Vim or Neovim; record the selected application and version | Open a synthetic UTF-8 file, insert and delete mixed-width text through ordinary and IME input, search, select with the mouse, resize repeatedly, save, and quit. Cursor, alternate screen, and prompt restore correctly. |  |  |
| RA-LINUX-PAGER | Pager | `less`; record version | Page through a synthetic long UTF-8/ANSI file, search forward and backward, follow wrapped lines, scroll with keys and wheel, resize, and quit. Text and prompt restore without stale lines. |  |  |
| RA-LINUX-TUI | Full-screen TUI or multiplexer | `tmux`; record version | Create two panes, run sustained bounded output in one pane and the editor in the other, enable mouse interaction, switch panes, resize the OdyTTY window, detach or exit, and confirm nested screen/mouse modes restore. |  |  |

## macOS

| ID | Category | Application | Bounded workload and expected result | Result | Evidence |
| --- | --- | --- | --- | --- | --- |
| RA-MACOS-SHELL | Default shell | The configured default shell, normally Zsh; record name and public version | Run synthetic Unicode output, a failing command, a pipeline, foreground interruption, and a subshell. Input, status, signals, resize, clipboard, and final `exit` remain correct. |  |  |
| RA-MACOS-EDITOR | Editor | Vim or Neovim; record the selected application and version | Open a synthetic UTF-8 file, insert and delete mixed-width text through ordinary and native IME input, search, select with the mouse, resize repeatedly, save, and quit. Cursor, alternate screen, and prompt restore correctly. |  |  |
| RA-MACOS-PAGER | Pager | `less`; record version | Page through a synthetic long UTF-8/ANSI file, search forward and backward, follow wrapped lines, scroll with keys and trackpad, resize, and quit. Text and prompt restore without stale lines. |  |  |
| RA-MACOS-TUI | Full-screen TUI or multiplexer | `tmux`; record version | Create two panes, run sustained bounded output in one pane and the editor in the other, enable mouse interaction, switch panes, resize or change display scale, detach or exit, and confirm nested screen/mouse modes restore. |  |  |

## Windows

| ID | Category | Application | Bounded workload and expected result | Result | Evidence |
| --- | --- | --- | --- | --- | --- |
| RA-WINDOWS-SHELL | Default shell | PowerShell 7 when installed, otherwise Windows PowerShell 5.1 or Command Prompt according to OdyTTY's documented preference | Run synthetic Unicode output, a failing native command, a pipeline, foreground interruption, and a child shell. ConPTY input, status, resize, clipboard, and final `exit` remain correct. |  |  |
| RA-WINDOWS-EDITOR | Editor | Vim or Neovim for Windows; record the selected application and version | Open a synthetic UTF-8 file, insert and delete mixed-width text through ordinary and native IME input, search, select with the mouse, resize repeatedly, save, and quit. Cursor, alternate screen, and prompt restore correctly. |  |  |
| RA-WINDOWS-PAGER | Pager | `more.com`; record the Windows version | Page through a synthetic long UTF-8 file, advance and interrupt, resize, and exit. Text remains aligned within the application's documented encoding behavior and the prompt restores without stale lines. |  |  |
| RA-WINDOWS-TUI | Full-screen TUI or multiplexer | `tmux` reached through Windows `ssh.exe` and the synthetic alias `odytty-manual-host`; record both public versions | Create two remote panes, run sustained bounded output in one pane and the editor in the other, enable mouse interaction, switch panes, resize the OdyTTY window, detach or exit, and confirm nested screen/mouse modes restore through ConPTY and SSH. |  |  |

## Completion rule

The matrix is complete only when every row has a result and evidence reference
for the exact release-candidate artifact. `FAIL` blocks the corresponding
platform gate. `SKIP` retains an evidence gap for final exception review.
`UNSUPPORTED` is valid only when it matches a documented OdyTTY platform
limitation, not when a test application or environment was unavailable.
