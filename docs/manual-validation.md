# Release-profile manual validation

This checklist defines the human, release-profile evidence required for Linux,
macOS, and Windows. It is a blank evidence format, not a completed validation
record. Empty result cells do not imply success.

The bounded real-program subset is defined separately in the
[real-application smoke matrix](compatibility/real-application-smoke.md). Run
that matrix with the same release-candidate artifact and link its results from
the platform record below.

The checklist covers behavior that automated tests cannot establish on their
own: native-window integration, real input methods and clipboards, text quality,
perceived interaction quality, GPU selection and fallback behavior, display
transitions, suspend and restore, and sustained use on physical systems.

Current expected behavior comes from the
[feature reference](features.md), [settings guide](settings-guide.md),
[keybindings](keybindings.md), [accessibility guide](accessibility.md),
[installation and GPU notes](install.md), and
[HiDPI matrix](hidpi-validation.md). The
[pre-1.0 acceptance contract](pre-1.0-acceptance.md) defines how completed
records contribute to release readiness. This checklist supplied the detailed
evidence boundary for the v0.10.0 decision and remains the reusable template for
later releases; each later milestone must state which rows are required rather
than inheriting a pass implicitly. When behavior changes, update the applicable
public reference and this checklist together before collecting new results.

## Result vocabulary

Every executed check uses exactly one of these values:

| Result | Meaning |
| --- | --- |
| `PASS` | The check was performed on the recorded artifact and matched the expected behavior. |
| `FAIL` | The check was performed and any expected behavior was missing, incorrect, unstable, or unsafe. |
| `SKIP` | The check applies to the platform but was not performed. The reason and resulting evidence gap are required. |
| `UNSUPPORTED` | The tested revision intentionally does not provide the behavior on that platform. The current public limitation and its evidence reference are required. |

Do not use `PASS` for an unobserved result. A missing application, input method,
second display, compositor, GPU path, or test host is `SKIP`, not
`UNSUPPORTED`. A documented product limitation is `UNSUPPORTED`, not `SKIP`.
An unexpected product failure remains `FAIL`; it must not be discarded as an
invalid attempt.

Automated evidence may establish the build, commit, or a deterministic state
transition. It cannot populate a manual result or replace human confirmation of
feel, text quality, native-window behavior, clipboard integration, IME behavior,
or physical GPU and display behavior. In particular, a successful
`windows-latest` run is automated Windows evidence only; it is not Windows feel
or hardware approval.

## Run record

Create one record for each platform and artifact. Do not combine observations
from different commits, builds, operating-system installations, or machines.

| Field | Required entry |
| --- | --- |
| Record ID | A stable synthetic identifier, such as `MV-LINUX-20260730-01` |
| Checklist revision | Full commit SHA containing this checklist |
| OdyTTY commit | Full 40-character commit SHA under test |
| OdyTTY version | Value reported by the release-profile binary |
| Source state | `CLEAN` or `DIRTY`, recorded before the build |
| Build command | Exactly `cargo build --release --locked` |
| Artifact kind | Source-built binary or named release package |
| Artifact identity | Public filename, byte size, and SHA-256 digest |
| Embedded identity | Commit shown in About or Copy diagnostics |
| Platform | `Linux`, `macOS`, or `Windows` |
| OS public class | Distribution and version, macOS major version, or Windows edition/version/build; no machine identity |
| Architecture | For example `x86_64` or `aarch64` |
| Native window stack | Desktop and Wayland/X11 compositor class, macOS window server, or Windows desktop compositor |
| Display class | Display count, resolution class, refresh-rate class, and scale factors |
| Hardware class | Desktop/laptop/virtual-machine class, CPU architecture class, and RAM range |
| GPU class | Vendor and product family, driver public version, backend, and hardware/software classification |
| Input class | Keyboard layout, pointer/touchpad class, and IME family used |
| Shells exercised | Shell names and public versions |
| Configuration | Default or a repository-relative sanitized configuration reference |
| Started | UTC date and time |
| Completed | UTC date and time |
| Overall result | `PASS`, `FAIL`, `SKIP`, or `UNSUPPORTED` |
| Evidence index | References to sanitized observations for every check |
| Skips | Check IDs, reasons, and the evidence gaps they leave |
| Unsupported behavior | Check IDs and matching public limitation references |
| Limitations | Hardware, environment, coverage, and observation limitations |
| Human confirmation role | `project maintainer` for a platform gate |
| Human confirmation date | UTC calendar date |

A `DIRTY` source build may be used for investigation, but it cannot close a
platform gate. The executable under test must be freshly produced after the
exact commit and source state are recorded; an older binary in `target` is not
acceptable.

### Build and identity commands

Use the platform-appropriate identity commands before opening the application.
Record sanitized results rather than raw command output.

Linux:

```sh
git rev-parse HEAD
git status --short
cargo build --release --locked
target/release/odytty --version
sha256sum target/release/odytty
```

macOS:

```sh
git rev-parse HEAD
git status --short
cargo build --release --locked
target/release/odytty --version
shasum -a 256 target/release/odytty
```

Windows PowerShell:

```powershell
git rev-parse HEAD
git status --short
cargo build --release --locked
.\target\release\odytty.exe --version
Get-FileHash .\target\release\odytty.exe -Algorithm SHA256
```

After the window opens, compare the commit shown by About or Copy diagnostics
with the recorded full commit. A mismatch is `FAIL`.

## Evidence record

Each result cell points to a sanitized evidence entry with this schema:

| Field | Required entry |
| --- | --- |
| Check ID | Exact row ID from this document |
| Result | `PASS`, `FAIL`, `SKIP`, or `UNSUPPORTED` |
| Observation date | UTC calendar date |
| Artifact digest | SHA-256 digest from the run record |
| Expected | The row's expected behavior |
| Observed | A concise factual description |
| Evidence reference | An anchor in the appended run record, a repository issue number, or a repository-relative sanitized artifact |
| Limitation | Anything the observation cannot prove |
| Failure reference | Required for `FAIL`; public issue number or security-report reference |
| Confirmation date | UTC calendar date |

Evidence must contain synthetic terminal content only. It must not contain
usernames, hostnames, device serial numbers, hardware UUIDs, network addresses,
personal filesystem paths, private host aliases or URLs, environment dumps,
shell history, clipboard contents, SSH credentials, tokens, or terminal output
copied from real work. Use placeholders such as `<REPO>`, `<CONFIG_DIR>`, and
the synthetic SSH alias `odytty-manual-host`.

Record concise, sanitized observations. Do not commit raw logs, raw diagnostic
dumps, or unredacted screen captures. Exact machine identity is neither needed
nor acceptable; a public-safe hardware class and the fields above are
sufficient.

## Common preparation

Perform these steps separately for each platform:

1. Use a clean checkout at the exact commit and run the required release build.
2. Record the artifact digest and confirm its embedded commit identity.
3. Start from a new, synthetic configuration and state directory. Preserve the
   original user state outside the test environment.
4. Prepare synthetic text containing ASCII, combining marks, wide CJK
   characters, emoji, box drawing, ligature candidates, right-to-left text, and
   long wrapped lines. Do not use personal content.
5. Prepare synthetic PNG, JPEG, GIF, and terminal graphics fixtures within the
   documented resource limits.
6. Use a designated non-sensitive SSH test system referenced only by the
   synthetic alias `odytty-manual-host`. Do not publish its address, account, or
   authentication material.
7. Exercise the effects-off path with `render_quality = plain`. Exercise the
   effects-on path separately with defaults, then reduced motion.
8. Keep automated test results separate from the manual record.

## Linux checklist

Record the Linux run fields before populating this table. Unless a row states
otherwise, it applies to both Wayland and X11. If only one display stack is
available, exercise it and mark the other stack's row `SKIP` with a reason.

| ID | Check | Expected behavior | Result | Evidence |
| --- | --- | --- | --- | --- |
| L-START-01 | Native startup | The release binary opens one responsive native window, a readable prompt, and the expected initial grid without a blank or corrupted first frame. |  |  |
| L-START-02 | Window close | The native close control exits promptly, releases the shell process tree, and leaves no OdyTTY window or child shell running. |  |  |
| L-START-03 | Shell exit | `exit` and end-of-file follow the configured pane, tab, workspace, and application close policy without a hang, duplicate window, or lost surviving pane. |  |  |
| L-SHELL-01 | Bash | An interactive Bash prompt accepts input, reports exit status correctly, runs full-screen programs, and restores the prompt after exit. |  |  |
| L-SHELL-02 | Zsh | An interactive Zsh prompt accepts input, reports exit status correctly, runs full-screen programs, and restores the prompt after exit. |  |  |
| L-SHELL-03 | Fish | An interactive Fish prompt accepts input, negotiates its keyboard behavior, runs full-screen programs, and restores the prompt after exit. |  |  |
| L-TAB-01 | Tabs | Create, switch, rename, reorder, and close several tabs; focus, title, content, and inline graphics remain attached to the correct session. |  |  |
| L-PANE-01 | Panes | Split horizontally and vertically, move focus, zoom, equalize, drag dividers, and close panes; input and rendering remain clipped to the intended pane. |  |  |
| L-WORK-01 | Workspaces | Create, name, switch, reorder, and close workspaces; the rail, active tab, and pane focus remain consistent. |  |  |
| L-WORK-02 | Close escalation | Closing the last pane closes its tab, the last tab closes its workspace, and the last workspace follows the configured application-exit behavior. |  |  |
| L-WORK-03 | Workspace restore | With restore enabled, a bare launch restores names, tab order, split shape, ratios, and valid working directories using fresh shells without replaying commands, output, environment, or scrollback. |  |  |
| L-CLIP-01 | Selection and text clipboard | Character, word, line, rectangular, and drag-extended selections copy and paste exact synthetic text; bracketed paste remains one transaction. |  |  |
| L-CLIP-02 | PRIMARY selection | Selection ownership and middle-click paste behave correctly on a Linux stack that provides PRIMARY, without replacing the regular clipboard unexpectedly. |  |  |
| L-CLIP-03 | OSC 52 write consent | Default `ask` prompts only for the focused active PTY; allow-once, allow-for-session, deny-for-session, cancel, background denial, and unfocused denial behave as documented. |  |  |
| L-CLIP-04 | OSC 52 read opt-in | The default sends no clipboard reply; explicit read opt-in returns only the synthetic clipboard value to the requesting active PTY. |  |  |
| L-CLIP-05 | Clipboard image | Pasting a synthetic clipboard image into an integrated remote tab prompts before upload, honors cancel, refuses an over-cap image, and on confirmation returns a remote path without typing it into the shell. Local and plain-SSH tabs remain unaffected. |  |  |
| L-RESIZE-01 | Resize and reflow | Repeated slow and rapid window resizes preserve logical content, cursor position, selection, alternate-screen state, inline graphics placement, and current PTY dimensions. |  |  |
| L-RESIZE-02 | Scale transition | Moving between available integer or fractional display scales rerasterizes crisp text and updates grid geometry without stale pixels or a crash. |  |  |
| L-TEXT-01 | IME | Pre-edit text appears at the cursor, the candidate window follows the caret, commit sends one correct string, and cancel sends nothing. |  |  |
| L-TEXT-02 | Unicode | Combining marks, wide characters, emoji, right-to-left text, box drawing, and mixed-width editing remain aligned through typing, selection, resize, and scroll. |  |  |
| L-TEXT-03 | Font quality | Regular, bold, italic, bold-italic, ligatures, symbol fallback, emoji, underlines, and cursor shapes are crisp, legible, baseline-aligned, and free of clipping or tofu where coverage is expected. |  |  |
| L-POINTER-01 | Pointer and wheel | Click selection, multi-click selection, drag selection, drag autoscroll, high-resolution scrolling, wheel notches, pane-divider drag, and tab/workspace drag behave without stuck capture or unintended focus changes. |  |  |
| L-POINTER-02 | Terminal mouse protocols | X10, normal, button-event, any-event, SGR, SGR-pixel, focus reporting, and alternate-scroll behavior reach a compatible TUI with correct buttons, modifiers, coordinates, press, motion, and release. |  |  |
| L-LINK-01 | Links and paths | OSC 8 links, allowlisted printed URLs, and enabled interactive paths show the expected hover state and open only after the documented modifier action; terminal output never opens them automatically. |  |  |
| L-IMAGE-01 | Terminal images | Bounded Kitty, Sixel, and iTerm2 inline images decode, place, scroll, clip, and clear correctly in a single pane and in splits without covering unrelated panes or text layers incorrectly. |  |  |
| L-IMAGE-02 | In-app image view | A supported synthetic image path opens in OdyTTY only when enabled, stays aspect-correct and bounded through resize and scale changes, and dismisses with the documented actions. |  |  |
| L-PLAIN-01 | Effects-off plain path | `render_quality = plain` produces stable readable text and interaction with post-processing, background treatment, dimming, per-cell stem darkening, and the contrast lift disabled as documented. |  |  |
| L-EFFECT-01 | Bounded effects-on path | Default effects never obscure text, change terminal state, delay logical input, leak across pane clips, or continue scheduling frames after motion settles. Reduced motion produces the documented static alternatives. |  |  |
| L-GPU-01 | Accelerated adapter | About reports the expected hardware adapter and backend; startup, sustained output, resize, images, and effects remain stable without device-loss artifacts. |  |  |
| L-GPU-02 | GPU fallback | On a prepared environment without the preferred Vulkan path, accelerated GL/GLES or a documented software renderer starts with an honest adapter classification and usable plain rendering. |  |  |
| L-LIFE-01 | Minimize and restore | Repeated minimize and restore preserves content, focus routing, scale, clipboard behavior, and a valid GPU surface. |  |  |
| L-LIFE-02 | Suspend and resume | System suspend and resume restores the native window, input, PTY flow, compositor surface, and GPU resources without stale content or runaway CPU use. |  |  |
| L-LIFE-03 | Wayland integration | On Wayland, focus, scale changes, clipboard/PRIMARY availability, window attention, and live system appearance changes match compositor capabilities. |  |  |
| L-LIFE-04 | X11 integration | On X11, focus, scaling, clipboard/PRIMARY behavior, window attention, and configured appearance seeding match documented capabilities. |  |  |
| L-SESS-01 | Detached session lifecycle | Create, list, detach, attach, replace, deduplicate, and terminate a synthetic detached session; the live terminal model survives client detach and closes at the documented boundary. |  |  |
| L-SESS-02 | Session restore | A live detached session reattaches when its saved host remains available; a missing host falls back to a fresh shell without replaying commands or output. |  |  |
| L-SSH-01 | Connection manager | Add, edit, filter, connect, and remove a synthetic saved host; opt-in OpenSSH import exposes bounded host metadata only and leaves unrelated configuration text unchanged. |  |  |
| L-SSH-02 | SSH lifecycle | Connect, open repeated tabs, resize, drop, reconnect, and close the synthetic SSH session; reuse, integration, and optional remote tmux follow the recorded settings and degrade to plain SSH safely. |  |  |
| L-SHELLINT-01 | Shell integration | Bash and Zsh prompt marks, working-directory updates, command-status gutter, click-to-position, editable-input deletion, and prompt-scoped key enhancements activate only inside the intended prompt. |  |  |
| L-SHELLINT-02 | Command-output actions | In separate Bash, Zsh, and Fish sessions, the palette, context menu, and configured keybindings select/copy output and prompt-inclusive ranges, scope search, and navigate explicit failures; absent, partial, stale, resized, evicted, reset, alternate-screen, and synchronized-output boundaries fail closed. |  |  |
| L-EXPORT-01 | Command-output export | On Wayland and X11 independently, the native portal save dialog supports cancel, new-file, and overwrite flows. Exported synthetic text excludes controls, hyperlink targets, cwd metadata, and image payloads, respects the 32 MiB cap, leaves the chosen parent unchanged, and refuses a final symlink. |  |  |
| L-ACCESS-01 | Accessibility alternatives | Keyboard-only access reaches settings, tabs, panes, workspaces, search, command palette, connections, and close paths; reduced motion, static cursor/scroll settings, contrast controls, CVD modes, focus indication, and visual bell remain legible. |  |  |
| L-LONG-01 | Long-running behavior | During at least four hours of interactive shells, idle periods, output, tab/pane changes, resize, and suspend/restore, the application remains responsive with no visible unbounded growth, accumulating artifacts, stuck animation, or lost input. |  |  |

## macOS checklist

Use a fresh release-profile build on supported Apple Silicon hardware. Exercise
at least one Retina scale and any available external-display transition.

| ID | Check | Expected behavior | Result | Evidence |
| --- | --- | --- | --- | --- |
| M-START-01 | Native startup | The release binary opens one responsive native window, a readable prompt, and the expected initial grid without a blank or corrupted first frame. |  |  |
| M-START-02 | Window close | The native close control and application quit path exit promptly, release child shells, and leave no OdyTTY window or child shell running. |  |  |
| M-START-03 | Shell exit | `exit` and end-of-file follow the configured pane, tab, workspace, and application close policy without a hang or loss of surviving sessions. |  |  |
| M-SHELL-01 | Zsh | The default interactive Zsh prompt accepts input, reports exit status correctly, runs full-screen programs, and restores the prompt after exit. |  |  |
| M-SHELL-02 | Bash | An interactive Bash prompt accepts input, reports exit status correctly, runs full-screen programs, and restores the prompt after exit. |  |  |
| M-SHELL-03 | Fish | When installed, an interactive Fish prompt accepts input, negotiates its keyboard behavior, runs full-screen programs, and restores the prompt after exit. |  |  |
| M-TAB-01 | Tabs | Create, switch, rename, reorder, and close several tabs; focus, title, content, and inline graphics remain attached to the correct session. |  |  |
| M-PANE-01 | Panes | Split horizontally and vertically, move focus, zoom, equalize, drag dividers, and close panes; input and rendering remain clipped to the intended pane. |  |  |
| M-WORK-01 | Workspaces | Create, name, switch, reorder, and close workspaces; the rail, active tab, and pane focus remain consistent. |  |  |
| M-WORK-02 | Close escalation | Closing the last pane closes its tab, the last tab closes its workspace, and the last workspace follows the configured application-exit behavior. |  |  |
| M-WORK-03 | Workspace restore | With restore enabled, a bare launch restores names, tab order, split shape, ratios, and valid working directories using fresh shells without replaying commands, output, environment, or scrollback. |  |  |
| M-CLIP-01 | Selection and text clipboard | Character, word, line, rectangular, and drag-extended selections copy and paste exact synthetic text; bracketed paste remains one transaction. |  |  |
| M-CLIP-02 | PRIMARY selection | Record `UNSUPPORTED`: macOS has no PRIMARY-selection surface. Regular clipboard behavior remains unaffected. |  |  |
| M-CLIP-03 | OSC 52 write consent | Default `ask` prompts only for the focused active PTY; allow-once, allow-for-session, deny-for-session, cancel, background denial, and unfocused denial behave as documented. |  |  |
| M-CLIP-04 | OSC 52 read opt-in | The default sends no clipboard reply; explicit read opt-in returns only the synthetic clipboard value to the requesting active PTY. |  |  |
| M-CLIP-05 | Clipboard image | Pasting a synthetic clipboard image into an integrated remote tab prompts before upload, honors cancel, refuses an over-cap image, and on confirmation returns a remote path without typing it into the shell. Local and plain-SSH tabs remain unaffected. |  |  |
| M-RESIZE-01 | Resize and reflow | Repeated slow and rapid window resizes preserve logical content, cursor position, selection, alternate-screen state, inline graphics placement, and current PTY dimensions. |  |  |
| M-RESIZE-02 | Retina and display transition | Moving between available display scales rerasterizes crisp text and updates grid geometry without stale pixels, blur, or a crash. |  |  |
| M-TEXT-01 | IME | Pre-edit text appears at the cursor, the candidate window follows the caret, commit sends one correct string, and cancel sends nothing. |  |  |
| M-TEXT-02 | Unicode | Combining marks, wide characters, emoji, right-to-left text, box drawing, and mixed-width editing remain aligned through typing, selection, resize, and scroll. |  |  |
| M-TEXT-03 | Font quality | Regular, bold, italic, bold-italic, ligatures, symbol fallback, color emoji, underlines, and cursor shapes are crisp, legible, baseline-aligned, and free of clipping or unexpected tofu. |  |  |
| M-POINTER-01 | Pointer and wheel | Click selection, multi-click selection, drag selection, drag autoscroll, high-resolution trackpad scrolling, wheel notches, pane-divider drag, and tab/workspace drag behave without stuck capture or unintended focus changes. |  |  |
| M-POINTER-02 | Terminal mouse protocols | X10, normal, button-event, any-event, SGR, SGR-pixel, focus reporting, and alternate-scroll behavior reach a compatible TUI with correct buttons, modifiers, coordinates, press, motion, and release. |  |  |
| M-LINK-01 | Links and paths | OSC 8 links, allowlisted printed URLs, and enabled interactive paths show the expected hover state and open only after the documented Command-modified action; terminal output never opens them automatically. |  |  |
| M-IMAGE-01 | Terminal images | Bounded Kitty, Sixel, and iTerm2 inline images decode, place, scroll, clip, and clear correctly in a single pane and in splits without covering unrelated panes or text layers incorrectly. |  |  |
| M-IMAGE-02 | In-app image view | A supported synthetic image path opens in OdyTTY only when enabled, stays aspect-correct and bounded through resize and scale changes, and dismisses with the documented actions. |  |  |
| M-PLAIN-01 | Effects-off plain path | `render_quality = plain` produces stable readable text and interaction with post-processing, background treatment, dimming, per-cell stem darkening, and the contrast lift disabled as documented. |  |  |
| M-EFFECT-01 | Bounded effects-on path | Default effects never obscure text, change terminal state, delay logical input, leak across pane clips, or continue scheduling frames after motion settles. Reduced motion produces the documented static alternatives. |  |  |
| M-GPU-01 | Metal adapter | About reports the expected Apple hardware adapter and Metal backend; startup, sustained output, resize, images, and effects remain stable without device-loss artifacts. |  |  |
| M-GPU-02 | Software GPU fallback | Record `UNSUPPORTED` on supported physical macOS systems: Metal is hardware-backed and OdyTTY documents no ordinary software-adapter path. Record virtualization separately if used. |  |  |
| M-LIFE-01 | Minimize and restore | Repeated minimize and restore preserves content, focus routing, scale, clipboard behavior, and a valid GPU surface. |  |  |
| M-LIFE-02 | Sleep and wake | System sleep and wake restores the native window, input, PTY flow, window-server surface, and GPU resources without stale content or runaway CPU use. |  |  |
| M-LIFE-03 | Native-window integration | Focus, application switching, close and quit, window attention, fullscreen, Retina scaling, and live system appearance changes behave consistently with macOS conventions. |  |  |
| M-SESS-01 | Detached session lifecycle | Create, list, detach, attach, replace, deduplicate, and terminate a synthetic detached session; the owner-private Unix socket remains local and the live terminal model survives client detach. |  |  |
| M-SESS-02 | Session restore | A live detached session reattaches when its saved host remains available; a missing host falls back to a fresh shell without replaying commands or output. |  |  |
| M-SSH-01 | Connection manager | Add, edit, filter, connect, and remove a synthetic saved host; opt-in OpenSSH import exposes bounded host metadata only and leaves unrelated configuration text unchanged. |  |  |
| M-SSH-02 | SSH lifecycle | Connect, open repeated tabs, resize, drop, reconnect, and close the synthetic SSH session; reuse, integration, and optional remote tmux follow the recorded settings and degrade to plain SSH safely. |  |  |
| M-SHELLINT-01 | Shell integration | Bash and Zsh prompt marks, working-directory updates, command-status gutter, click-to-position, editable-input deletion, and prompt-scoped key enhancements activate only inside the intended prompt. |  |  |
| M-SHELLINT-02 | Command-output actions | In separate Bash, Zsh, and Fish sessions, the palette, context menu, and configured keybindings select/copy output and prompt-inclusive ranges, scope search, and navigate explicit failures; absent, partial, stale, resized, evicted, reset, alternate-screen, and synchronized-output boundaries fail closed. |  |  |
| M-EXPORT-01 | Command-output export | The native macOS save dialog supports cancel, new-file, and overwrite flows. Exported synthetic text excludes controls, hyperlink targets, cwd metadata, and image payloads, respects the 32 MiB cap, leaves the chosen parent unchanged, and refuses a final symlink. |  |  |
| M-ACCESS-01 | Accessibility alternatives | Keyboard-only access reaches settings, tabs, panes, workspaces, search, command palette, connections, and close paths; reduced motion, static cursor/scroll settings, contrast controls, CVD modes, focus indication, and visual bell remain legible. |  |  |
| M-LONG-01 | Long-running behavior | During at least four hours of interactive shells, idle periods, output, tab/pane changes, resize, and sleep/wake, the application remains responsive with no visible unbounded growth, accumulating artifacts, stuck animation, or lost input. |  |  |

## Windows checklist

Use the x86_64 release-profile binary on a supported Windows system with
ConPTY. Record whether PowerShell 7 is installed; the default-shell preference
is PowerShell 7, then Windows PowerShell 5.1, then `cmd.exe`.

Windows process containment has an important observation limit. When assignment
to the kill-on-close Job succeeds, closing the PTY uses whole-tree
`TerminateJobObject`. If assignment fails, OdyTTY degrades to root-only
`TerminateProcess`. A manual child-tree close check observes the outcome but
cannot prove which internal path executed without separate sanitized
diagnostics. Record that limitation; do not claim unconditional whole-tree
containment from a single successful close.

| ID | Check | Expected behavior | Result | Evidence |
| --- | --- | --- | --- | --- |
| W-START-01 | Native startup | The release binary opens one responsive native window, a readable prompt, and the expected initial grid without a blank or corrupted first frame. |  |  |
| W-START-02 | ConPTY spawn and exit | A default ConPTY session starts the selected shell, accepts input, resizes, reports shell exit, and reaches output EOF without a hang. |  |  |
| W-START-03 | Window close and child tree | Closing the native window exits promptly. A synthetic child tree exits when Job containment is active; any surviving descendant is `FAIL`, and the Job-assignment limitation is retained in the evidence. |  |  |
| W-START-04 | Shell exit | `exit` and end-of-file follow the configured pane, tab, workspace, and application close policy without a hang or loss of surviving sessions. |  |  |
| W-SHELL-01 | PowerShell 7 | When installed, `pwsh.exe` accepts Unicode input, preserves native exit status, runs full-screen programs, and restores the prompt after exit. |  |  |
| W-SHELL-02 | Windows PowerShell 5.1 | Windows PowerShell accepts Unicode input, preserves native exit status, runs console programs, and restores the prompt after exit. |  |  |
| W-SHELL-03 | Command Prompt | `cmd.exe` accepts input, expands its ordinary environment, runs console programs, and restores the prompt after exit. |  |  |
| W-SHELL-04 | Unicode path and environment | Launch from a synthetic path containing spaces and non-ASCII characters, set a synthetic Unicode environment value, and confirm the shell, OSC 7 working directory, open-path handling, and child process receive it without corruption. |  |  |
| W-KEY-01 | ConPTY Win32 input mode | A compatible console application that requests private mode 9001 receives correct key-down and key-up records, virtual keys, scan codes, Unicode text, modifiers, and repeat counts. Legacy input remains correct when the mode is inactive. |  |  |
| W-TAB-01 | Tabs | Create, switch, rename, reorder, and close several tabs; focus, title, content, and inline graphics remain attached to the correct session. |  |  |
| W-PANE-01 | Panes | Split horizontally and vertically, move focus, zoom, equalize, drag dividers, and close panes; input and rendering remain clipped to the intended pane. |  |  |
| W-WORK-01 | Workspaces | Create, name, switch, reorder, and close workspaces; the rail, active tab, and pane focus remain consistent. |  |  |
| W-WORK-02 | Close escalation | Closing the last pane closes its tab, the last tab closes its workspace, and the last workspace follows the configured application-exit behavior. |  |  |
| W-WORK-03 | Workspace persistence | With restore enabled, a bare launch restores names, tab order, split shape, ratios, and valid working directories using fresh ConPTY shells without replaying commands, output, environment, or scrollback. |  |  |
| W-WORK-04 | Live-session persistence limit | Record `UNSUPPORTED`: Windows workspace snapshots store no detached session-host IDs and always restore fresh shells. |  |  |
| W-CLIP-01 | Selection and text clipboard | Character, word, line, rectangular, and drag-extended selections copy and paste exact synthetic text; bracketed paste remains one transaction. |  |  |
| W-CLIP-02 | PRIMARY selection | Record `UNSUPPORTED`: Windows has no PRIMARY-selection surface. Regular clipboard behavior remains unaffected. |  |  |
| W-CLIP-03 | OSC 52 write consent | Default `ask` prompts only for the focused active PTY; allow-once, allow-for-session, deny-for-session, cancel, background denial, and unfocused denial behave as documented. |  |  |
| W-CLIP-04 | OSC 52 read opt-in | The default sends no clipboard reply; explicit read opt-in returns only the synthetic clipboard value to the requesting active PTY. |  |  |
| W-CLIP-05 | Clipboard image | Pasting a synthetic clipboard image into an integrated remote tab prompts before upload, honors cancel, refuses an over-cap image, and on confirmation returns a remote path without typing it into the shell. Local and plain-SSH tabs remain unaffected. |  |  |
| W-DROP-01 | External file drag and drop | Record `UNSUPPORTED` while the tested revision has no external OS file-drop route. Internal tab, workspace, divider, and selection drags are covered separately and must not be cited as file-drop support. |  |  |
| W-RESIZE-01 | Resize and reflow | Repeated slow and rapid native-window resizes preserve logical content, cursor position, selection, alternate-screen state, inline graphics placement, and current ConPTY dimensions. |  |  |
| W-RESIZE-02 | Scale transition | Moving between available integer or fractional display scales rerasterizes crisp text and updates grid geometry without stale pixels, blur, or a crash. |  |  |
| W-TEXT-01 | IME | Pre-edit text appears at the cursor, the candidate window follows the caret, commit sends one correct string, and cancel sends nothing. |  |  |
| W-TEXT-02 | Unicode | Combining marks, wide characters, emoji, right-to-left text, box drawing, and mixed-width editing remain aligned through typing, selection, resize, and scroll. |  |  |
| W-TEXT-03 | Font quality | Regular, bold, italic, bold-italic, ligatures, symbol fallback, monochrome emoji fallback, underlines, and cursor shapes are crisp, legible, baseline-aligned, and free of clipping or unexpected tofu. |  |  |
| W-POINTER-01 | Pointer and wheel | Click selection, multi-click selection, drag selection, drag autoscroll, precision-touchpad scrolling, wheel notches, pane-divider drag, and tab/workspace drag behave without stuck capture or unintended focus changes. |  |  |
| W-POINTER-02 | Terminal mouse protocols | X10, normal, button-event, any-event, SGR, SGR-pixel, focus reporting, and alternate-scroll behavior reach a compatible TUI with correct buttons, modifiers, coordinates, press, motion, and release. |  |  |
| W-LINK-01 | Links and paths | OSC 8 links, allowlisted printed URLs, and enabled drive-absolute, UNC, and backslash-relative paths show the expected hover state and open only after the documented Control-modified action. Drive-relative paths remain undetected as documented. |  |  |
| W-IMAGE-01 | Terminal images | Bounded Kitty, Sixel, and iTerm2 inline images decode, place, scroll, clip, and clear correctly in a single pane and in splits. Unsupported POSIX shared-memory graphics transport is recorded separately rather than treated as a decode failure. |  |  |
| W-IMAGE-02 | In-app image view | A supported synthetic image path opens in OdyTTY only when enabled, stays aspect-correct and bounded through resize and scale changes, and dismisses with the documented actions. |  |  |
| W-PLAIN-01 | Effects-off plain path | `render_quality = plain` produces stable readable text and interaction with post-processing, background treatment, dimming, per-cell stem darkening, and the contrast lift disabled as documented. |  |  |
| W-EFFECT-01 | Bounded effects-on path | Default effects never obscure text, change terminal state, delay logical input, leak across pane clips, or continue scheduling frames after motion settles. Reduced motion produces the documented static alternatives. |  |  |
| W-GPU-01 | Hardware adapter | About reports the expected hardware adapter and Direct3D 12 backend; startup, sustained output, resize, images, and effects remain stable without device-loss artifacts. |  |  |
| W-GPU-02 | WARP fallback | On a prepared WARP environment, About reports the Microsoft Basic Render Driver as software, startup warns honestly, and the plain path remains usable without presenting software rendering as hardware evidence. |  |  |
| W-LIFE-01 | Minimize and restore | Repeated minimize and restore preserves content, focus routing, scale, clipboard behavior, and a valid GPU surface. |  |  |
| W-LIFE-02 | Lock, sleep, and resume | Lock/unlock and system sleep/wake restore the native window, input, ConPTY flow, desktop-compositor surface, and GPU resources without stale content or runaway CPU use. |  |  |
| W-LIFE-03 | Native-window integration | Focus, application switching, close, window attention, fullscreen/maximize, taskbar behavior, and live scale changes behave consistently with Windows conventions. |  |  |
| W-SESS-01 | Detached sessions | Record `UNSUPPORTED`: `odytty new --detached`, live session listing/attach, and the managed-session host are Unix-only. The in-window manager remains empty without pretending that a live session exists. |  |  |
| W-SESS-02 | Restore semantics | Restored workspaces create fresh ConPTY shells and retain shape only; they do not claim to reconnect a detached or live local process. |  |  |
| W-SSH-01 | Connection manager | Add, edit, filter, connect, and remove a synthetic saved host; opt-in OpenSSH import exposes bounded host metadata only and leaves unrelated configuration text unchanged. |  |  |
| W-SSH-02 | SSH lifecycle | Using `ssh.exe`, connect, resize, drop, reconnect, and close the synthetic remote session. Integration and optional remote tmux follow the recorded settings and degrade to plain SSH safely. |  |  |
| W-SSH-03 | SSH reuse limit | Record `UNSUPPORTED`: Unix ControlMaster socket reuse is not emitted on Windows, so repeated connections authenticate independently. |  |  |
| W-SHELLINT-01 | PowerShell integration | PowerShell prompt marks, working-directory updates, command-status gutter, click-to-position, editable-input deletion, and enhanced key behavior activate only inside the intended prompt and preserve native exit codes. |  |  |
| W-SHELLINT-02 | Command Prompt integration | Record `UNSUPPORTED`: `cmd.exe` has no OSC 133 hook surface. Ordinary terminal input and output remain functional without integrated prompt features. |  |  |
| W-SHELLINT-03 | Command-output actions | In PowerShell, the palette, context menu, and configured keybindings select/copy output and prompt-inclusive ranges, scope search, and navigate explicit failures; absent, partial, stale, resized, evicted, reset, alternate-screen, and synchronized-output boundaries fail closed. Command Prompt remains unsupported. |  |  |
| W-EXPORT-01 | Command-output export | The native Windows save dialog supports cancel, new-file, and overwrite flows. Exported synthetic text excludes controls, hyperlink targets, cwd metadata, and image payloads, respects the 32 MiB cap, leaves the chosen parent unchanged, and refuses a final reparse point. |  |  |
| W-ACCESS-01 | Accessibility alternatives | Keyboard-only access reaches settings, tabs, panes, workspaces, search, command palette, connections, and close paths; reduced motion, static cursor/scroll settings, contrast controls, CVD modes, focus indication, and visual bell remain legible. |  |  |
| W-LONG-01 | Long-running behavior | During at least four hours of interactive shells, idle periods, output, tab/pane changes, resize, and lock/sleep/restore, the application remains responsive with no visible unbounded growth, accumulating artifacts, stuck animation, or lost input. |  |  |

## Human feel and hardware gates

These observations are mandatory for each shipped platform and must be made by
a human using the same fresh release-profile artifact identified in the run
record. An automated result, virtual framebuffer, remote-desktop capture, or
headless GPU test cannot satisfy them.

| Gate | Required observation | Linux | macOS | Windows |
| --- | --- | --- | --- | --- |
| HF-01 | Initial and sustained glyph rasterization is crisp and stable at the recorded scale, with readable weight, baseline, spacing, fallback, emoji, and box drawing. |  |  |  |
| HF-02 | Typing, key repeat, shortcuts, selection, scrolling, pane focus, and resize feel immediate and predictable; no visible presentation effect delays logical input. |  |  |  |
| HF-03 | IME pre-edit, candidate placement, commit, cancel, and mixed-script editing feel native and remain aligned with the cursor. |  |  |  |
| HF-04 | Clipboard, native focus, attention, minimize, restore, scale transitions, and compositor/window-server behavior match platform expectations. |  |  |  |
| HF-05 | The recorded hardware GPU path renders correctly under startup, output, resize, images, panes, and bounded effects. |  |  |  |
| HF-06 | The platform's documented fallback path is exercised and honest, or is recorded as `SKIP` with a blocking gap or `UNSUPPORTED` with the current limitation. |  |  |  |
| HF-07 | The effects-off plain path is readable and stable, and the effects-on path preserves text clarity and settles to idle. |  |  |  |
| HF-08 | The application remains comfortable and dependable through the recorded long-running session, including idle periods and system lifecycle transitions. |  |  |  |

A platform gate is `PASS` only when every applicable required check and every
human feel and hardware gate is `PASS`. Any `FAIL` blocks it. Any `SKIP`
preserves an open evidence gap unless a bounded exception is recorded under the
pre-1.0 acceptance contract. `UNSUPPORTED` is acceptable only when it matches
the tested revision's public platform scope, is listed in the run record, and
does not contradict a shipped claim.

## Failure routing

Preserve every `FAIL` in the run record, then:

1. Create a public issue containing the exact check ID, commit, artifact digest,
   public-safe platform class, minimal synthetic reproduction, expected
   behavior, observed behavior, impact, and sanitized evidence reference.
2. Route a disclosure-sensitive failure through the project's security
   reporting path instead of publishing an exploit, secret, private address, or
   hostile payload.
3. Link the issue or security reference from the failed check. Do not change the
   result to `SKIP` or `UNSUPPORTED`.
4. After a fix, create a new run record on the new commit. Keep the original
   failure and its limitations intact.
5. If the failure is carried temporarily, record the bounded exception,
   mitigation, responsible project role, and expiry date required by the
   pre-1.0 acceptance contract.

Skipped and unsupported checks follow the same traceability rule: state the
reason, the evidence gap or limitation, and the public reference. Silence is
not a result.
