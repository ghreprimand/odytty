# OdyTTY Terminal Threat Model

A terminal emulator is an interpreter for untrusted input. Every byte arriving
on the pseudoterminal is attacker-controlled the moment a user runs `cat` on a
downloaded file, pipes a hostile log, or connects to a compromised host over
SSH. This document enumerates each external-input and privilege boundary in
OdyTTY, states what an attacker controls at that boundary, what OdyTTY assumes,
what it caps, how it fails, and what risk remains.

This is a description of the code as it exists at the revision this document
landed, not a statement of intent. Where a mitigation is planned rather than
present, it is labeled as planned. Where a risk is unresolved, it is recorded
as unresolved rather than softened.

## Contents

- [Method and scope](#method-and-scope)
- [Threat actors and trust levels](#threat-actors-and-trust-levels)
- [Assets and security goals](#assets-and-security-goals)
- [Supported and unsupported boundaries](#supported-and-unsupported-boundaries)
- [Boundary catalog](#boundary-catalog)
- [Platform process and privilege boundaries](#platform-process-and-privilege-boundaries)
- [Test and fuzz coverage map](#test-and-fuzz-coverage-map)
- [Unresolved residual risks](#unresolved-residual-risks)
- [What this model does not cover](#what-this-model-does-not-cover)

## Method and scope

Each boundary below is described with nine fields:

| Field | Meaning |
| --- | --- |
| Attacker control | What an adversary can choose at this boundary |
| Trust assumption | What OdyTTY takes on faith, and why that is defensible |
| Current default | Shipped behavior with no configuration changes |
| Validation and caps | Enforced bounds, with source anchors |
| Failure behavior | What happens when a bound is exceeded or input is malformed |
| Diagnostic exposure | What reaches logs, and whether it can carry attacker data |
| Existing tests | Test coverage present today |
| Planned fuzz target | Coverage-guided target that should exist |
| Residual risk | What is still not addressed |

Source anchors are file paths and symbol names rather than line numbers where
the symbol is unambiguous, so the anchors survive ordinary refactoring. Facts
about current behavior are separated from proposed mitigations throughout: the
"planned" label marks everything that does not exist yet.

Protocol and platform semantics that are not self-evident are taken from
primary sources: the xterm control-sequence documentation, the Kitty graphics
protocol specification, the OpenSSH `ssh_config` manual page, POSIX, and
Microsoft's pseudoconsole and process-creation documentation.

## Threat actors and trust levels

Four actors, in descending order of assumed capability at the terminal
boundary:

**A1 — Hostile output producer.** Controls the byte stream written to the
pseudoterminal. This covers a remote host over SSH, a compromised or malicious
program run locally, a file whose contents get printed, and any network payload
that ends up on standard output. This is the primary actor: it is unprivileged
with respect to OdyTTY, it is remote in the common case, and it requires no
user mistake beyond displaying data. Every escape-sequence, graphics, clipboard,
hyperlink, and shell-integration boundary faces A1.

**A2 — Hostile local file.** Controls the contents of a file OdyTTY reads:
configuration, theme, session state, workspace snapshot, font, image, SSH
configuration, or connection-host list. A2 is same-user in the common case, so
it is materially weaker than A1 — a same-user attacker can usually replace the
binary. A2 matters because these files can be synchronized from elsewhere,
restored from a backup, or shared, and because a parse failure that panics or
allocates without bound is a defect regardless of who wrote the file.

**A3 — Hostile co-resident process.** Runs as another user on the same
machine, or as the same user with fewer privileges. Relevant to shared-memory
graphics transports, temporary-file transports, session sockets, and runtime
state directories.

**A4 — Hostile input device or window-system peer.** Supplies clipboard
contents, window-system events, or display-server messages. Weak in practice
because the window system is inside the trust boundary of the session, but the
clipboard is an untrusted channel: its contents originate anywhere.

The user running OdyTTY is trusted. Anything the user can already do without
OdyTTY — read their own files, run arbitrary programs — is not a vulnerability
when OdyTTY does it on their instruction. The interesting failures are those
where A1 through A4 cause an effect the user did not request.

## Assets and security goals

The assets worth protecting, in priority order:

1. **The user's ability to run commands deliberately.** The single worst
   failure mode for a terminal is executing something the user did not type.
   Nothing displayed on screen may become a command.
2. **Local file confidentiality.** Terminal output must not be able to read
   arbitrary local files and transmit their contents back over the same stream.
3. **Clipboard integrity.** Terminal output must not silently overwrite the
   clipboard, and must not read it.
4. **Credential material.** SSH keys, key paths, and authentication tokens must
   not be read, logged, or transmitted by OdyTTY on terminal instruction.
5. **Availability.** Displaying hostile output must not hang, crash, or exhaust
   the machine.
6. **Session isolation.** One session's state must not be readable or
   controllable from another user's process.

The corresponding goals:

- **G-EXEC:** no path from terminal output to command execution without an
  explicit human gesture.
- **G-READ:** no path from terminal output to reading an arbitrary local path.
- **G-CLIP:** no silent clipboard write, no clipboard read, by default.
- **G-CRED:** no credential material is parsed, stored, or logged.
- **G-AVAIL:** every untrusted length, count, and payload is bounded.
- **G-ISO:** session state is owner-only and ownership is verified.

## Supported and unsupported boundaries

**In scope.** Pseudoterminal output parsing; escape, control, and string
sequences; clipboard sequences; hyperlink sequences and the external opener;
inline graphics in every transport; image decoding; shell integration fields;
environment handling at spawn; SSH configuration import and connection
invocation; session sockets, metadata, and state files; settings, theme, and
workspace persistence; font loading; resource exhaustion; and the process and
privilege differences between Linux, macOS, and Windows.

**Out of scope, by design.**

- **A same-user attacker who can already write to the OdyTTY binary, its
  configuration directory, or its state directory.** Such an attacker has
  already won by simpler means. Boundaries against A2 are hardening, not a
  security perimeter.
- **The window system and display server.** A compromised compositor can read
  the screen and synthesize input; no terminal can defend against that.
- **The shell and the programs the user runs.** OdyTTY does not sandbox child
  processes, and does not claim to.
- **Network transport security for SSH.** Connection security belongs to the
  `ssh` binary that OdyTTY invokes. OdyTTY builds argv; it does not implement
  the protocol, parse keys, or handle authentication.
- **Cryptographic guarantees of any kind.** OdyTTY ships no cryptography.

## Boundary catalog

### B1 — Hostile pseudoterminal output, UTF-8 decoding, and escape parsing

The widest boundary and the one an attacker reaches with no user action beyond
displaying data.

- **Attacker control:** every byte, at any chunk boundary, in any order, at any
  rate, indefinitely. Split multi-byte sequences across reads are fully under
  attacker control.
- **Trust assumption:** none. The parser treats the stream as adversarial and
  is expected to remain in a well-defined state for every byte sequence,
  including invalid UTF-8 and truncated sequences.
- **Current default:** parsing is always on; there is no reduced-trust mode.
  Reads use a fixed 8 KiB stack buffer (`src/native/pty.rs`).
- **Validation and caps:** the two-layer pipeline separates UTF-8 handling from
  control parsing. `src/parser/segmenter.rs` owns ground state and all UTF-8,
  carrying partial code points across chunk boundaries without byte loss;
  `src/parser/machine.rs` is an 8-bit-clean control automaton with a flat
  state/class transition table, so no input reaches an undefined state.
  Parameters are bounded at `MAX_PARAMS` = 32 with saturating accumulation and
  no heap allocation (`src/parser/params.rs`). String payloads are bounded:
  operating-system commands at `MAX_OSC_RAW` = 128 KiB and at most
  `MAX_OSC_PARAMS` = 16 parameters (`src/parser/driver.rs`); application program
  commands at `MAX_APC_RAW` = 1 MiB, dropped rather than truncated when
  exceeded, so a partial payload is never dispatched as if complete; device
  control strings stream through without parser buffering, and device-control
  query responses are bounded at `MAX_DCS_QUERY_BYTES` = 4096
  (`src/core/screen/query.rs`). Keyboard-protocol mode stacking is bounded at
  `KITTY_KEYBOARD_STACK_LIMIT` = 16 (`src/core/screen/ops.rs`). Scrollback is
  bounded at `DEFAULT_SCROLLBACK_LIMIT` = 10,000 lines with a per-logical-line
  ceiling of `MAX_LOGICAL_LINE_CELLS` = 2^20 (`src/core/scrollback.rs`).
  Combining marks per cell are capped at `MAX_COMBINING` = 4
  (`src/core/types.rs`). Program-defined clickable regions are bounded at
  `MAX_BUTTON_SPANS_PER_LINE` = 16, `MAX_BUTTON_ENTRIES` = 8192, and
  `MAX_CODE_DIGITS` = 10 (`src/core/button.rs`).
- **Failure behavior:** over-cap string payloads are dropped and the parser
  returns to ground. Invalid UTF-8 produces replacement characters rather than
  a decode error. Out-of-range parameters saturate. No input path is expected to
  panic; a panic here would be a defect, not a documented failure mode.
- **Diagnostic exposure:** the parser does not log payload bytes. Screen content
  never reaches the log by this path.
- **Existing tests:** a parser transition oracle asserting byte-identical screen
  state across a fixture corpus fed whole and at every byte split
  (`src/core/parser_oracle_tests.rs`); state-machine and segmenter unit suites
  (`src/parser/machine_tests.rs`, `src/parser/segmenter_tests.rs`,
  `src/parser/params_tests.rs`, `src/parser/driver_tests.rs`); three
  deterministic protocol fuzzers with a configurable iteration budget and a
  deep tier behind an ignore attribute (`tests/protocol_fuzz.rs`).
- **Planned fuzz target:** a coverage-guided target over the segmenter and
  machine pair with a retained public corpus, asserting no panic, bounded
  allocation, and identical final state for whole versus split feeds. The
  existing deterministic fuzzers are seed-driven, not coverage-guided; this is
  the gap.
- **Residual risk:** the deterministic fuzzers explore a structured space, so
  coverage is shaped by their generators. Until coverage-guided fuzzing with a
  retained corpus exists, unexplored transition combinations cannot be ruled
  out. Sustained maximum-rate output is bounded per-payload but not
  rate-limited overall; a fast producer can keep the parser busy indefinitely,
  which is a responsiveness concern rather than a memory one.

### B2 — Clipboard write and read sequences (OSC 52)

The sequence that lets terminal output set the system clipboard. Historically
the source of clipboard-injection attacks in other terminals, where a clipboard
write plants a command that the user later pastes into a shell.

- **Attacker control:** the selection target and the full base64 payload; the
  timing; unlimited repetition.
- **Trust assumption:** none. A clipboard write is treated as a request, not an
  instruction.
- **Current default:** reads are **denied**. `osc52_read` defaults to `false`
  (`src/settings.rs`), so a clipboard-read request is refused regardless of the
  write policy — terminal output cannot exfiltrate clipboard contents. Writes go
  through an explicit policy type (`Osc52WritePolicy`, `src/settings.rs`) and a
  native authority check.
- **Validation and caps:** payload bounded at `OSC52_CLIPBOARD_MAX_BYTES` =
  64 KiB with bounded base64 decoding (`src/core/screen/mod.rs`). Terminal
  parsing stays model-only: the core never touches the system clipboard. The
  final authority check lives in `src/native/app/osc52.rs` and requires that the
  emitting pseudoterminal is still active, that the application window is still
  focused, and that the live write policy permits the request. Consent decisions
  are tracked per session (allow once, allow for the session, deny for the
  session, cancel). Notices are rate-limited at one per second
  (`NOTICE_RATE_LIMIT`).
- **Failure behavior:** over-cap payloads and invalid base64 are discarded. A
  write from an unfocused window or a dead session is discarded. A denied read
  produces no reply at all rather than an empty one, so the absence of clipboard
  access is not itself a probe signal.
- **Diagnostic exposure:** clipboard payload bytes are not logged.
- **Existing tests:** screen-level clipboard sequence tests under
  `src/core/tests`; native policy and consent-state tests in
  `src/native/app/osc52.rs`.
- **Planned fuzz target:** covered by the operating-system-command arm of the
  planned parser target; the base64 decoder should be reachable from it.
- **Residual risk:** the focus requirement is a strong mitigation but not a
  complete one — output rendered while the window is focused is the normal case,
  and a user who leaves a hostile process running in a focused window can still
  be prompted repeatedly. The rate limit bounds nuisance, not intent.

### B3 — Hyperlinks (OSC 8) and external openers

Where displayed data becomes a URL and a URL becomes a spawned process. The
highest-consequence boundary in the model, because it is the shortest path from
attacker-chosen bytes to process execution.

- **Attacker control:** the full URI, the link identifier, the displayed text,
  and the number of distinct links. Displayed text and destination need not
  agree, so the visible label is not evidence of the target.
- **Trust assumption:** the user's explicit modified click is the consent
  gesture. Nothing else opens a link.
- **Current default:** links are recorded and underlined on hover, and open only
  on an explicit modified click — Ctrl and click on Linux and Windows, Command
  and click on macOS. There is no plain-click open, no hover-open, and no
  automatic open.
- **Validation and caps:** URIs bounded at `MAX_URI_BYTES` = 2083; the link
  table bounded at `MAX_TABLE_BYTES` = 4 MiB and `MAX_LINK_ENTRIES` = 8192 with
  a per-entry overhead charge of `ENTRY_OVERHEAD_BYTES` = 64, so link storage
  cannot be inflated by many tiny entries (`src/core/hyperlink.rs`). A scheme
  allowlist — `http`, `https`, `file`, `mailto`, matched case-insensitively —
  gates opening (`uri_has_openable_scheme`); everything else, including
  `javascript:`, is refused. The opener path is argv-only: every opener builds a
  string vector and never constructs a shell command line
  (`src/native/app/platform_opener.rs`), with a single spawn point
  (`src/native/app/interactive_paths.rs`). Platform dispatch is keyed on an
  explicit enumeration rather than scattered compile-time conditionals, so the
  macOS and Windows argv branches are unit-tested on any host. Desktop-entry
  reads for the open-with path are bounded at `MAX_DESKTOP_FILE_BYTES` =
  256 KiB, and content-type sniffing reads at most `MIME_SNIFF_BYTES` = 32 bytes
  (`src/native/app/open_with_ui.rs`, `src/native/app/platform_opener.rs`).
- **Failure behavior:** a disallowed scheme, an over-length URI, or an
  unresolvable target does not open and does not spawn. Unknown editor
  specifications degrade to opening the file without position rather than
  guessing a flag that could be read as a filename.
- **Diagnostic exposure:** a refused open can be logged with the scheme; full
  URIs are attacker-controlled and should not be logged at levels enabled by
  default.
- **Existing tests:** scheme-allowlist tests including the case-insensitivity
  and `javascript:` rejection cases (`src/core/hyperlink.rs`); argv-construction
  tests for all three platform branches
  (`src/native/app/platform_opener.rs`).
- **Planned fuzz target:** a URI-shaped target over scheme parsing plus argv
  construction, asserting that no generated input produces a shell
  metacharacter-sensitive command line on any platform branch.
- **Residual risk:** `file:` is on the allowlist, so a modified click can open a
  local path chosen by terminal output through the platform default handler.
  This is the documented behavior — it is what makes clickable paths useful —
  but it means the consent gesture is the only barrier between hostile output
  and the platform's file-type handler. Displayed text can misrepresent the
  destination; the model relies on the hover treatment to disclose the real
  target.

### B4 — Inline graphics payloads (direct transport)

- **Attacker control:** the full base64 payload, declared dimensions, format,
  chunk count, and placement parameters.
- **Trust assumption:** none. Declared dimensions are treated as claims and
  validated against the actual payload.
- **Current default:** the direct inline transport is enabled; it carries data
  in-band and touches no filesystem state.
- **Validation and caps:** pending encoded payload bounded at
  `MAX_PENDING_ENCODED_BYTES` = 96 MiB across chunks (`src/core/kitty.rs`), so a
  chunked transmission cannot grow without limit. The decoded image store
  applies its own bound of 64 MiB (`src/graphics/store.rs`). Sixel decoding is
  bounded at `MAX_WIDTH` and `MAX_HEIGHT` = 10,000, `MAX_PIXELS` = 40,000,000,
  `MAX_COLOR_REG` = 1024 color registers, and `MAX_PARAM` = 99,999,999
  (`src/graphics/sixel.rs`). Dimension arithmetic uses checked multiplication
  because a `u32` squared fits in `u64` but the four-byte-per-pixel product does
  not (`src/graphics/store.rs`).
- **Failure behavior:** over-cap or malformed payloads produce a protocol error
  response and are discarded; the image is not partially placed.
- **Diagnostic exposure:** payload bytes are not logged.
- **Existing tests:** `src/core/kitty_tests.rs`,
  `src/core/kitty_delete_tests.rs`, `src/core/graphics_tests.rs`,
  `src/graphics/sixel_tests.rs`, `src/graphics/store_tests.rs`,
  `src/graphics/placement_tests.rs`, and a graphics fuzz suite
  (`src/core/graphics_fuzz_tests.rs`).
- **Planned fuzz target:** a coverage-guided target over the graphics envelope
  and Sixel decoder with a retained public corpus, bounded allocation, and
  deterministic crash reproduction.
- **Residual risk:** the 96 MiB pending bound is generous. A stream that
  repeatedly approaches it produces sustained allocation churn without ever
  tripping a cap.

### B5 — Named and shared-memory graphics transports

- **Attacker control:** the object name, and — for a co-resident attacker (A3) —
  the object contents.
- **Trust assumption:** that a named object inside the allowed prefix set was
  created by a cooperating program. This assumption is why the transport is off
  by default.
- **Current default:** **disabled.** `kitty_named_transports` defaults to
  `false` (`src/settings.rs`); named transports require explicit opt-in. Shared
  memory is Unix-only: the non-Unix implementation returns a transport error
  because `shm_open` and `mmap` have no portable analogue
  (`src/core/kitty_transport.rs`).
- **Validation and caps:** reads bounded at `MAX_TRANSPORT_READ_BYTES` = 96 MiB,
  enforced before any decode attempt, so a small file claiming enormous
  dimensions cannot become a decode bomb. Objects are opened read-only, then
  validated for size and content, and only then unlinked — a rejected object
  keeps its name rather than being destroyed by a failed read. Paths must be
  valid UTF-8 with no embedded null bytes.
- **Failure behavior:** validation failure produces the standard protocol error
  response. Rejected objects are not unlinked.
- **Diagnostic exposure:** transport errors carry a category, not payload
  content.
- **Existing tests:** `src/core/kitty_transport_tests.rs`.
- **Planned fuzz target:** a name-shaped target over path validation, asserting
  that no generated name escapes the allowed prefix set on either platform
  family.
- **Residual risk:** off by default, so the residual risk applies only to
  opt-in users. A co-resident attacker who can create an object in the allowed
  prefix set before the legitimate producer does can substitute pixel data. The
  read-only-then-unlink ordering limits this to content substitution rather than
  destruction.

### B6 — Temporary-file and ordinary-file graphics transports

The transport where terminal output names a host path and OdyTTY reads it. The
mechanism most directly opposed to G-READ, and deliberately narrowed relative to
the reference implementation.

- **Attacker control:** the path string, and whether a deletion marker is
  present.
- **Trust assumption:** that files inside the system temporary directories are
  not sensitive. This is why the allowlist exists rather than a general path
  filter.
- **Current default:** disabled together with the named transports
  (`kitty_named_transports` = `false`).
- **Validation and caps:** reads are restricted to a canonical temporary-
  directory allowlist — `/tmp`, `/dev/shm`, a canonicalizable `TMPDIR`, and on
  Windows the canonicalized system temporary directory (`allowed_temp_dirs`,
  `src/core/kitty_transport.rs`). Containment is verified by canonicalizing the
  *parent* directory and checking the prefix, because the file itself may not
  exist at validation time. Files are opened with `O_NOFOLLOW` on Unix so the
  kernel rejects symlinks. Windows opens the final component with
  `FILE_FLAG_OPEN_REPARSE_POINT` and rejects an opened handle carrying
  `FILE_ATTRIBUTE_REPARSE_POINT`. Both checks prevent a final-component link
  from being followed before the regular-file check. The temporary-file
  transport additionally requires the
  reference `tty-graphics-protocol` marker in the path and deletes the file
  before decode, so a decode failure still leaves nothing behind; rejected
  special files are never deleted. The size cap is applied to the raw file read
  before any decode.
- **Failure behavior:** a path outside the allowlist, a symlink, a missing
  marker, an oversized file, or a special file is refused with a protocol error
  and no read.
- **Diagnostic exposure:** transport errors name a category. Paths are
  attacker-controlled and should not be logged by default.
- **Existing tests:** `src/core/kitty_transport_tests.rs` covers path
  validation, prefix containment, symlink rejection, marker enforcement, and
  cap behavior.
- **Planned fuzz target:** as B5, plus a path-shaped target exercising
  parent-canonicalization against traversal and mixed-separator inputs.
- **Residual risk:** named transports remain off by default. The resolved
  final-component behavior is recorded as finding **E** below.

### B7 — Image decoding and resource bounds

- **Attacker control:** the full encoded byte stream and every header field,
  including declared dimensions and compression parameters.
- **Trust assumption:** none. File extensions are not trusted; content is
  sniffed.
- **Current default:** decoding is enabled for inline graphics and for the image
  viewer path.
- **Validation and caps:** decoding runs under an explicit limits object —
  `MAX_IMAGE_DIM` = 12,000 pixels per axis and `MAX_IMAGE_ALLOC_BYTES` = 256 MiB
  total decode allocation (`src/native/image_decode.rs`) — applied identically
  to every decode call through a single helper, so no decode site can be missed.
  Format is determined by content sniffing rather than by filename, so a text
  file named with an image extension is classified by its bytes. The post-decode
  graphics store applies its own 64 MiB bound on what is actually uploaded, and
  background images are separately bounded at `MAX_BG_IMAGE_DIM` = 4096
  (`src/native/gpu/image.rs`).
- **Failure behavior:** a missing, unreadable, unidentifiable, undecodable, or
  oversized image returns nothing and never panics, so a bad path cannot crash
  the renderer.
- **Diagnostic exposure:** decode failures are logged as a category without
  payload content.
- **Existing tests:** decode-bound tests in `src/native/image_decode.rs`;
  graphics fuzz coverage in `src/core/graphics_fuzz_tests.rs`.
- **Planned fuzz target:** a coverage-guided decoder target with a retained
  public corpus of malformed encodings, run under bounded memory.
- **Residual risk:** decoding delegates to a third-party image library. The
  limits object bounds allocation but does not bound decode *time*; a
  pathological but in-bounds image can consume CPU. Advisory exposure through
  the image and font dependency graph is tracked by the dependency audit gate,
  not by this document.

### B8 — Clipboard channel, paste, and drag-and-drop

- **Attacker control:** clipboard contents (A4), which may include control
  characters and newlines intended to execute on paste.
- **Trust assumption:** paste is a user-initiated gesture, but its *content* is
  untrusted.
- **Current default:** bracketed paste is used where the application enables it,
  which is the standard mitigation against paste-executes-immediately.
  Drag-and-drop of files onto the window is **not implemented**: no
  window-system file-drop event is handled anywhere in the source, so this
  boundary has no attack surface today. It is recorded here so a future
  implementation inherits an explicit contract rather than starting from
  silence.
- **Validation and caps:** paste payloads bounded at
  `MAX_BRACKETED_PASTE_BYTES` = 32 MiB and chunked at `PASTE_CHUNK_SIZE` =
  16 KiB so bracketed framing can never tear mid-payload
  (`src/native/clipboard.rs`, `src/native/pty.rs`). An over-cap paste is refused
  whole rather than truncated, because delivering a truncated body is the
  dangerous option — a partial command line is still a command line.
- **Failure behavior:** an over-cap paste is refused with a notice; no partial
  bytes are written to the pseudoterminal.
- **Diagnostic exposure:** paste contents are not logged.
- **Existing tests:** paste chunking, framing, and cap tests in
  `src/native/clipboard.rs`.
- **Planned fuzz target:** a paste-shaped target asserting that chunk framing is
  never split across a bracketed-paste boundary for any input length.
- **Residual risk:** the platform clipboard library acquires its RGBA buffer
  before handing it to OdyTTY; dimensions, raw bytes, and PNG output are bounded
  immediately after that handoff (finding **B**). Bracketed paste depends on the
  receiving application enabling it; a shell that does not is outside OdyTTY's
  control.

### B9 — Shell integration, environment, and process launch

- **Attacker control:** for A1, the contents of shell-integration report
  sequences — working directory, command text, exit status. For A2, the contents
  of the integration wrapper files.
- **Trust assumption:** integration fields are display and navigation data, not
  instructions. A reported working directory changes where a new tab starts; it
  never causes execution.
- **Current default:** integration wrappers are written into OdyTTY's own
  configuration directory and the detected shell is pointed at them; if any part
  fails, the command is left unchanged so shell startup never depends on
  integration plumbing (`src/shell_integration.rs`). Windows integration is
  provided for PowerShell.
- **Validation and caps:** working-directory reporting parses a
  `file://host/path` form, percent-decodes the path, and accepts only an empty
  or local host — a remote host in the field is not resolved or contacted.
  Drive-letter working directories are handled on Windows. Prompt marks are
  bounded by the same string caps as other operating-system commands (B1).
  Snapshot string fields are bounded at `DEFAULT_MAX_STRING_BYTES` = 4096
  (`src/core/snapshot_envelope.rs`).
- **Failure behavior:** an unparseable or non-local working-directory report is
  ignored. A failed integration write leaves the spawn command untouched.
- **Diagnostic exposure:** integration failures are logged as categories.
  Reported command text is attacker-controlled and must not be logged by
  default.
- **Existing tests:** `src/shell_integration.rs` unit tests; prompt-mark tests
  in `src/core/prompt_marks.rs`.
- **Planned fuzz target:** a field-shaped target over working-directory and
  prompt-mark parsing, asserting no panic and no path escape for arbitrary
  percent-encoded input, with Windows drive-letter and separator forms included.
- **Residual risk:** a hostile remote can set an arbitrary local-looking working
  directory, so a new tab may open somewhere unexpected. This is a usability
  surprise rather than an execution path, but it is real.

### B10 — SSH configuration import and connection invocation

The boundary that touches credential-adjacent data, and therefore the one with
the most deliberate omissions.

- **Attacker control:** for A2, the contents of an imported configuration file.
  For A1, nothing directly — terminal output cannot trigger a connection.
- **Trust assumption:** the configuration file is a display source, not an
  execution source.
- **Current default:** OdyTTY **never discovers a configuration path on its
  own** and **never follows `Include`** (`src/ssh_config.rs`). A caller must
  pass the exact path or bytes. Only quick-connect display fields are surfaced:
  `Host` aliases plus optional `HostName`, `User`, and `Port`. Key-material
  directives such as `IdentityFile` are ignored entirely — not parsed, not
  stored, not displayed.
- **Validation and caps:** `DEFAULT_SSH_CONFIG_MAX_BYTES` = 256 KiB,
  `DEFAULT_SSH_CONFIG_MAX_ENTRIES` = 1024, and
  `DEFAULT_SSH_CONFIG_MAX_FIELD_CHARS` = 512. Connection invocation is
  argv-only, never a shell string (`src/ssh_connect.rs`). Reachability probes
  use `BatchMode=yes` so no authentication prompt can be triggered by a probe,
  with probe error output bounded at `PROBE_STDERR_CAP` = 8 KiB
  (`src/native/app/connection_probe.rs`).
- **Failure behavior:** over-cap files are truncated at the entry level rather
  than partially parsed into malformed entries; unparseable lines are skipped.
- **Diagnostic exposure:** host aliases and user names are personal data.
  Connection diagnostics must not record a combined user-and-host string at
  default log levels.
- **Existing tests:** parser and limit tests in `src/ssh_config.rs`; argv
  construction and probe classification tests in `src/ssh_connect.rs`.
- **Planned fuzz target:** a configuration-shaped target asserting bounded
  parsing, no panic, and no field leakage across entries for arbitrary bytes.
- **Residual risk:** low. The connection-host parser and every mutation path now
  share the bounded regular-file policy recorded in finding **D**.

### B11 — Session sockets, metadata, and state files

- **Attacker control:** for A3, attempts to connect to or squat on the session
  socket. For A2, the contents of metadata and snapshot files.
- **Trust assumption:** the runtime directory is owner-only and owner-verified.
- **Current default:** detachable session hosting is **Unix-only**; the module
  is compile-time gated and has no Windows implementation
  (`src/session_host/mod.rs`).
- **Validation and caps:** the runtime directory is created with mode 0700 and
  then validated: it must be a directory (checked with a non-following stat) and
  it must be owned by the current effective user, or startup fails
  (`src/session_host/socket.rs`). A stale path is only removed after confirming
  it is actually a socket — a non-socket path at the socket location is refused
  rather than deleted. Snapshot persistence is bounded at `MAX_SNAPSHOT_BYTES` =
  8 MiB, `MAX_WORKSPACES` = 512, `MAX_TABS_PER_WORKSPACE` = 512,
  `MAX_PANE_DEPTH` = 48, `MAX_TOTAL_LEAVES` = 8192
  (`src/native/persistence.rs`), with parse depth bounded at `MAX_PARSE_DEPTH` =
  128 (`src/native/persistence/json.rs`). State reads stop at the byte cap plus
  one, so a file that grows after its descriptor-length check is still rejected
  without an unbounded allocation. The host's PTY reader feeds a fixed 256-event
  queue (about 2 MiB at the 8 KiB read size), and each host-loop pass processes
  at most that many events before returning to client input, shutdown, child-exit,
  and idle handling. A continuously-writing child therefore receives normal PTY
  backpressure instead of growing userspace memory or starving host control flow.
  The writer contract distinguishes a
  zero-progress send timeout (drop the frame, keep the stream) from a
  partial-progress timeout (the stream is desynchronized and tears down
  visibly), so a stalled peer cannot silently desynchronize the protocol
  (`src/session_host/protocol.rs`). Cell and row wire sizes are fixed and
  bounded (`src/core/snapshot_envelope.rs`).
- **Failure behavior:** ownership or type validation failure aborts rather than
  proceeding. A corrupt metadata file causes that one session to list with
  reduced information rather than aborting enumeration.
- **Diagnostic exposure:** session names are user-chosen and may be personal;
  paths in error context include the runtime directory.
- **Existing tests:** `src/session_host/tests.rs`, including deterministic
  stalling-writer tests that pin the progress-versus-teardown distinction;
  `src/session_host/host.rs` asserts the PTY queue capacity and per-pass fairness
  budget; `src/native/persistence/tests.rs` asserts the exact state-read byte
  boundary and boundary plus one.
- **Planned fuzz target:** a state-file target over metadata and snapshot
  parsing, asserting fail-closed behavior with no panic, no unbounded
  allocation, and no file access outside the runtime directory.
- **Residual risk:** low. Metadata reads now use the bounded, owner-validated,
  final-component-nonfollowing boundary recorded in finding **A**.

### B12 — Settings, theme, and workspace files

- **Attacker control:** for A2, full file contents.
- **Trust assumption:** none beyond same-user provenance.
- **Current default:** every configuration and theme read goes through one
  bounded helper.
- **Validation and caps:** `read_capped` bounds every read at
  `MAX_CONFIG_BYTES` = 1 MiB by reading at most the cap plus one byte, so an
  oversized file is detected without loading it (`src/settings/fs_read.rs`).
  Non-regular files produce an invalid-data error rather than blocking on a
  device. Warning accumulation is bounded at `MAX_WARNINGS` = 100 so a hostile
  file cannot produce unbounded log output on the reload thread. **The cap and
  behavior are identical on Linux, macOS, and Windows** — this is stated in the
  module and is a deliberate uniformity guarantee, not an accident of the Unix
  path.
- **Failure behavior:** an over-cap file is not loaded at all; the previous
  settings remain in effect. Parse failures degrade to defaults per field rather
  than rejecting the whole file.
- **Diagnostic exposure:** warnings name the setting and the problem, not the
  full file contents.
- **Existing tests:** `src/settings/fs_read.rs` cap tests and the suites under
  `src/settings/tests`.
- **Planned fuzz target:** a settings-shaped target asserting fail-closed
  parsing with no panic and no unintended file access, including Windows path
  and encoding forms.
- **Residual risk:** low. The same cap-plus-one and regular-file pattern now
  covers the sibling readers closed in findings A, C, and D.

### B13 — Hostile fonts

- **Attacker control:** for A2, the full font file — tables, offsets, lengths,
  and glyph programs.
- **Trust assumption:** installed system fonts are assumed well-formed. This
  assumption is weak: font files are complex binary formats parsed by
  third-party code.
- **Current default:** system font enumeration reads candidate files to extract
  family metadata, and the configured font is loaded at startup.
- **Validation and caps:** parsing is memory-safe Rust (`ttf-parser` for
  metadata, `swash` for shaping and rasterization, `ab_glyph` vector types for
  loading), and a parse failure returns an error rather than panicking
  (`src/text/face_meta.rs`). Every production whole-font read shares the 256 MiB
  regular-file boundary in `src/font_file.rs`; glyph rasterization output is
  bounded by atlas capacity.
- **Failure behavior:** an unparseable font is skipped during enumeration or
  falls back to the next candidate at load time.
- **Diagnostic exposure:** font paths may include a user's home directory and
  must not be logged at default levels.
- **Existing tests:** glyph corpus and rasterization smoke tests
  (`tests/glyph_corpus.rs`, `tests/stem_raster_smoke.rs`,
  `tests/emoji_pixel_smoke.rs`).
- **Planned fuzz target:** a font-shaped target over metadata extraction with a
  corpus of malformed and truncated font files, run under bounded memory.
- **Residual risk:** the metadata parser's informational unmaintained-dependency
  advisory is time-bounded through 2026-10-15 by the dependency audit gate and
  documented in `docs/release.md`. Finding **C** records the closed file-read
  boundary rather than an outstanding unbounded allocation.

### B14 — Resource exhaustion

Cross-cutting. Availability failures rarely come from one boundary; they come
from a bound that was applied to one path and missed on a sibling.

- **Attacker control:** volume, rate, and repetition at every boundary above.
- **Current defaults and caps:** the per-boundary caps listed throughout, plus
  the pseudoterminal write queue bounded at `QUEUE_BYTE_CAP` = 4 MiB
  (`src/native/pty_writer.rs`), output recording bounded at `MAX_FRAMES` = 600
  and `MAX_BYTES` = 24 MiB (`src/native/output_recorder.rs`), directory listing
  bounded at `MAX_DIR_ENTRIES` = 1024 (`src/native/settings_panel/path_picker.rs`),
  and picker result lists bounded at 40 entries across the overlay surfaces.
  Frame-recreation retry is bounded at `MAX_SKIPPED_FRAME_RECREATES` = 2 and
  `MAX_SKIPPED_RETRIES` = 8 (`src/native/app/mod.rs`) so a failing device cannot
  produce an unbounded retry loop.
- **Failure behavior:** bounded degradation. Frames are dropped rather than
  queued without limit; over-cap payloads are refused rather than truncated.
- **Existing tests:** cap tests colocated with each bounded module.
- **Planned mitigation:** continue systematic sibling-path review. Findings A,
  C, and D were instances of a guard applied to one of several parallel
  operations while its twins were missed; their closures route the sibling
  paths through shared bounded readers.
- **Residual risk:** unbounded *time* is less well covered than unbounded
  memory. Several boundaries cap allocation without capping work.

## Platform process and privilege boundaries

### Linux

Child processes are launched through OdyTTY's own pseudoterminal layer using
`openpt`, `grantpt`, `unlockpt`, and `TIOCGPTPEER`, with the window size set via
`TIOCSWINSZ`. Children become session leaders with a controlling terminal, so
the POSIX foreground process group is available and signal delivery follows
standard semantics. Master end-of-file behavior is normalized. Session sockets
are Unix domain sockets in a 0700 owner-verified runtime directory.

### macOS

The same POSIX pseudoterminal path as Linux. The opener is the `open` command
(and `open -R` for reveal) rather than the freedesktop utilities; application
enumeration for the open-with path uses the platform workspace interface, which
is content-type aware, so the magic-byte sniffing fallback has no caller there.
Session hosting follows the Unix path.

### Windows

Windows behavior is stated explicitly rather than inferred from the Unix path,
because the process model differs at the foundation.

- **Process creation.** There is no fork-and-exec, no controlling terminal, and
  no process group. A child is launched with `CreateProcessW`, attaching the
  pseudoconsole through the pseudoconsole thread attribute on an extended
  startup information structure (`src/pty/windows.rs`). Because the shape
  differs, the Unix pseudoterminal-pair entry point has no Windows analogue and
  stays compile-time gated; pseudoconsole creation happens inside the spawn
  path instead.
- **Argument handling.** Command lines are built from argument vectors through
  the standard library's own quoting. That quoting is correct for the documented
  `CommandLineToArgvW` rules but does **not** escape command-interpreter
  metacharacters. Therefore untrusted strings are never routed through the
  command interpreter: the default-open path uses an argument-vector form
  targeting the file explorer specifically for this reason
  (`src/native/app/platform_opener.rs`).
- **Console-window suppression.** OdyTTY ships as a graphical-subsystem binary
  with no attached console, so spawning a console child with no creation flags
  would allocate a fresh console and flash a window. Every such spawn — SSH
  probes, SSH uploads, openers — routes through one helper that sets
  `CREATE_NO_WINDOW` (`src/native/app/win_spawn.rs`). The flag is a harmless
  no-op for graphical children, so the helper guards every spawn site uniformly.
  A new spawn site that bypasses it is a defect.
- **Process containment.** Terminating a child tree uses a per-session job
  object with the kill-on-job-close limit, which is the pseudoconsole answer to
  the missing POSIX process-group kill. This is **best-effort**: if job creation
  or assignment failed at spawn time, termination degrades to terminating the
  root process only, and descendants may survive. There is no Windows equivalent
  of the POSIX foreground process group, so foreground-job detection always
  reports an unknown state — the documented safe default the shared contract
  already treats as safe to close.
- **Environment and paths.** Environment variables are inherited and augmented
  at spawn. Path handling accommodates drive letters throughout, including
  drive-letter working directories reported by shell integration, and file
  reference construction converts separators and inserts the leading slash the
  URI form requires so that a drive letter is never parsed as a URI authority
  (`src/paths/file_uri.rs`).
- **Session persistence.** Detachable session hosting is **not available on
  Windows.** The session-host module is Unix-gated in its entirety; there is no
  detached host process, no Unix domain socket, and no resumable session. A
  Windows user's sessions end with the window.
- **Connection multiplexing.** Connection reuse through `ControlMaster`,
  `ControlPersist`, and socket multiplexing is compiled out on Windows because
  the OpenSSH client there does not implement it (`src/ssh_connect.rs`). Windows
  connections are therefore independent; the socket-directory boundary that
  exists on Unix has no Windows counterpart.
- **Graphics transports.** The shared-memory transport is unavailable and
  returns a transport error. The temporary-file transport uses the canonicalized
  system temporary directory as its allowed prefix. The `O_NOFOLLOW` symlink
  rejection is Unix-only; see finding **E**.
- **Shell integration.** PowerShell integration emits the standard prompt-mark
  sequences. The integration wrapper mechanism used for Unix shells does not
  apply.

Windows correctness is verified by the blocking `windows-latest` continuous-
integration leg, which builds and runs the full automated suite. That leg is
authoritative for automated behavior on Windows and is the only automated
authority available. It cannot confirm manual, perceptual, or hardware-dependent
behavior, and a Linux or macOS result is never a substitute for a Windows one.

## Test and fuzz coverage map

| Boundary | Present today | Planned coverage-guided target |
| --- | --- | --- |
| B1 parser and UTF-8 | transition oracle, split-feed corpus, three deterministic fuzzers | segmenter and machine pair, retained public corpus |
| B2 clipboard sequences | screen and native policy tests | reached through the operating-system-command arm of B1 |
| B3 hyperlinks and openers | allowlist and three-platform argv tests | URI plus argv construction |
| B4 inline graphics | protocol, delete, store, placement, Sixel suites, graphics fuzz | graphics envelope and Sixel decoder |
| B5 named and shared memory | transport validation tests | transport name validation |
| B6 file transports | path, symlink, marker, cap tests | path validation including Windows forms |
| B7 image decoding | decode-bound tests | malformed-encoding corpus |
| B8 paste and drag-drop | chunking and framing tests | paste framing invariants |
| B9 shell integration | unit and prompt-mark tests | field parsing with Windows path forms |
| B10 SSH configuration | parser, limit, argv, probe tests | configuration parsing |
| B11 sessions and state | stalling-writer and protocol tests | metadata and snapshot parsing |
| B12 settings and themes | cap and parse tests | settings parsing with Windows forms |
| B13 fonts | glyph corpus and raster smoke tests | malformed font corpus |

Every planned target must retain a provenance-safe public corpus, run under
bounded allocation and bounded time, reproduce crashes deterministically, and
minimize each crash to a permanent regression fixture before the finding is
closed. No private terminal transcript is ever ingested as corpus material.

## Residual risks and completed closures

Five issues were identified by source inspection at the revision this document
describes and are stated at exactly the scope demonstrated — no exploitation
beyond that scope is claimed. Findings A through E are now closed with focused
tests. Each closure carries its own tests and sibling-path sweep.

### Finding A — Session metadata reads are bounded and reject final-component symlinks

- **Anchor:** `read_session_metadata` in `src/session_host/registry.rs` opens
  metadata through the owner/private sensitive-file boundary. On Unix the open
  uses `O_NOFOLLOW | O_NONBLOCK`, validates the opened descriptor as a regular
  file owned by the effective user, and reads no more than 64 KiB plus one
  detection byte.
- **Scope demonstrated:** focused tests accept valid metadata exactly at the
  cap, reject cap plus one, and reject a final-component symlink without reading
  or modifying its sibling target. The containing runtime directory remains
  mode 0700 and owner-validated (`src/session_host/socket.rs`).
- **Why the boundary matters:** descriptor validation prevents a pathname swap
  from turning a checked path into a different object, while the second
  cap-plus-one read check catches a regular file that grows after its metadata
  length is observed.
- **Unix and Windows:** Unix-only, because session hosting is Unix-only.
- **Status:** resolved for the session-metadata read boundary.

### Finding B — Clipboard image processing is bounded before PNG encoding

- **Anchor:** `read_image_png` in `src/native/clipboard.rs` validates the
  platform-provided dimensions and RGBA length against the shared 12,000-pixel
  and 256 MiB image limits before compression. PNG output writes through a
  fixed-cap buffer and stops one byte past the 10 MiB upload ceiling.
- **Scope demonstrated:** focused tests cover the exact raw-byte boundary,
  boundary plus one, excessive dimensions, malformed RGBA shape, and the PNG
  output detection byte. The platform clipboard API necessarily owns its RGBA
  acquisition before returning it; OdyTTY performs no further unbounded
  allocation or compression work after that handoff.
- **Why the boundary matters:** clipboard contents are an untrusted channel
  (A4). Oversized dimensions or buffers now stop before compression, while an
  incompressible image cannot grow the encoded buffer without limit.
- **Unix and Windows:** all platforms with clipboard image support.
- **Status:** resolved for OdyTTY-owned clipboard image processing after the
  platform clipboard handoff.

### Finding C — Font-file reads share one regular-file and size boundary

- **Anchors:** direct text-font loading, metadata enumeration, and emoji-font
  loading all route through `read_font_file` in `src/font_file.rs`.
- **Scope demonstrated:** the shared reader validates both the path metadata and
  the opened object as regular files, rejects a known oversized file before
  allocation, and stops at 256 MiB plus one detection byte if the file grows.
  Focused tests cover the exact cap boundary, non-regular paths, and the
  preserved symlink-to-regular-file behavior used by system font installs.
- **Why the boundary matters:** enumeration can inspect many filesystem entries
  during startup. One audited policy now bounds every production whole-font
  read before the parser receives owned bytes.
- **Unix and Windows:** all platforms; font enumeration runs on each.
- **Status:** resolved for production font-file reads.

### Finding D — Connection-host reads share one bounded regular-file policy

- **Anchors:** normal parsing plus `append_adhoc_host`, `edit_host_block`, and
  `remove_host_block` in `src/connection_hosts.rs` route through
  `read_hosts_file`. The parser retains its bounded-prefix behavior; mutations
  reject a file over 256 KiB because truncating unseen bytes would be unsafe.
- **Scope demonstrated:** the shared reader validates the path and opened object
  as regular files, uses a nonblocking open on Unix, and detects growth with one
  byte beyond the mutation ceiling. Focused tests cover the exact boundary,
  boundary plus one, and all three mutation paths leaving oversized input
  unchanged. Mutation output is also prevented from crossing the same ceiling.
- **Why the boundary matters:** parsing and mutation now apply one whole-file
  allocation policy, while editing continues to preserve every accepted byte.
- **Unix and Windows:** all platforms.
- **Status:** resolved for every production `hosts.conf` reader and mutation.

### Finding E — Windows file transports reject final-component reparse points

- **Anchor:** `read_regular_file` in `src/core/kitty_transport.rs` opens the
  final component with `FILE_FLAG_OPEN_REPARSE_POINT` on Windows, then rejects
  the opened handle when `FILE_ATTRIBUTE_REPARSE_POINT` is present. Unix keeps
  its `O_NOFOLLOW | O_NONBLOCK` descriptor boundary. `validate_path` separately
  canonicalizes and allowlists the parent directory on every platform.
- **Scope demonstrated:** a `cfg(windows)` integration test constructs a real
  file symlink inside the platform temporary directory, drives it through the
  public Kitty graphics surface, and requires the `symlink-rejected` response,
  no placed image, and an unchanged target. The test is part of the blocking
  `windows-latest` suite and fails rather than skipping when the runner cannot
  construct its fixture.
- **Why the boundary matters:** opening the reparse point itself makes the
  decision about the opened object rather than about a pathname checked before
  open. A final-component redirect therefore stops before size inspection or
  content read.
- **Unix and Windows:** Unix rejects final-component links at `open`; Windows
  rejects an opened final-component reparse-point handle. Other non-Unix
  platforms retain the regular-file check.
- **Status:** resolved in implementation and guarded by blocking Windows-only
  coverage; the Windows CI result remains the authoritative execution evidence.

## What this model does not cover

- **Quantitative risk.** No likelihood or impact scores are assigned. The
  findings above are ordered by boundary, not by severity.
- **Dependency advisories.** Third-party advisory exposure is tracked by the
  dependency audit gate and its recorded exceptions, not duplicated here.
- **Guarantees.** This document describes bounds that exist and gaps that
  remain. It does not assert that the enumerated boundaries are the complete
  set; a boundary absent from this catalog is an omission to be corrected, not
  an implicit statement that none exists.
- **Platform-specific verification.** Finding E carries a blocking Windows-only
  regression because Linux and macOS execution cannot establish Windows reparse-
  point behavior. Findings A through D carry the focused closure evidence stated
  above.
