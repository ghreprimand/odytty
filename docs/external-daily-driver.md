# Telemetry-free external daily-driver evidence program

Status: draft. This document defines a proposed evidence format and privacy
boundary. It does not authorize enrollment, begin an observation period, set
release thresholds, or contain field results.

The program exists to answer a narrow question: have people outside OdyTTY's
development group used one identified build as their ordinary terminal, on
their own hardware, for a sustained period, with unfavorable outcomes recorded
as carefully as favorable ones?

This evidence is distinct from automated tests, release-day smoke tests,
benchmarks, and the release-profile checklist in
[manual-validation.md](manual-validation.md). Those remain necessary, but none
of them demonstrates independent daily use.

The [pre-1.0 acceptance contract](pre-1.0-acceptance.md) defines the external
field gate, the [stabilization policy](stabilization-policy.md) defines
candidate restart expectations, and the [diagnostics guide](diagnostics.md)
defines the application's local logging boundary. This program applies a
stricter publication boundary to submitted evidence because operating-system
error text can contain private paths.

## Approval boundary

Collection must not begin until every value in the decision table below is
filled and the complete program is approved by the project maintainer. Until
then, this document is a proposal and the external-field-evidence gate remains
open.

| Decision | Required form | Approved value |
| --- | --- | --- |
| Candidate build | Exact version, full commit SHA, artifact filenames, and SHA-256 digests | `<UNSET>` |
| Total independent cohort size | Integer count of distinct qualifying people | `<UNSET>` |
| Per-platform cohort floor | Separate integer for Linux, macOS, and Windows | `<UNSET>` |
| Observation period | Integer number of consecutive weeks | `<UNSET>` |
| Minimum evidence per person | Required use days, duration or duration bands, daily entries, and workflow coverage | `<UNSET>` |
| Release-blocking threshold | Explicit mapping from issue severity, frequency, reproducibility, and platform scope to a release block | `<UNSET>` |
| Exception policy | Eligibility, required fields, approval role, mitigation, expiry, and maximum duration | `<UNSET>` |
| Candidate replacement rule | Which changes restart the full period beyond the mandatory behavior-relevant restart rule | `<UNSET>` |
| Withdrawal treatment | How withdrawn records remain represented in sanitized aggregates | `<UNSET>` |
| Publication location | Repository-relative location for accepted individual records and the final report | `<UNSET>` |

The values are deliberately unset. Example identifiers and templates elsewhere
in this document are structural examples, not proposed numbers or decisions.

## Privacy and consent invariants

The program is telemetry-free by construction:

- OdyTTY gains no analytics, telemetry, crash-reporting service, update ping,
  account, tracking identifier, background upload, or product-improvement
  network request.
- Enrollment and every submission are explicit human actions outside the
  application. Silence produces no record.
- The application does not generate a participant identifier. A public record
  uses a campaign-local synthetic code that is not reused across programs.
- No central private usage database is created. Accepted evidence lives in the
  declared public repository locations, except disclosure-sensitive security
  reports handled through the project's security-reporting path.
- Participation is optional. Refusal or withdrawal does not change product
  behavior, access, update availability, or support.
- Only the minimum public-safe fields in this document are requested.
- Terminal contents are never requested as daily-use evidence.

A repository host may expose its own account and request metadata according to
that host's policies. The OdyTTY program neither adds to that collection nor
copies host account metadata into field records.

### Explicit consent

Before the first accepted entry, the person must affirm all of the following:

1. Participation is voluntary and may stop at any time.
2. The submitted record is intended for public publication.
3. The entry contains no terminal content, secrets, personal paths, private
   system names, or third-party personal information.
4. The candidate identity and environment class are accurate to the person's
   knowledge.
5. Unfavorable observations will be retained and summarized with favorable
   observations.
6. Withdrawal cannot remove copies already mirrored, cached, or quoted outside
   the canonical repository.

Consent is recorded as `YES` with a UTC calendar date. No signature, legal
name, email address, or account identifier is requested in the evidence file.

## Qualifying independent use

Each record declares one relationship:

| Relationship | Meaning | Counts toward the external cohort |
| --- | --- | --- |
| `INDEPENDENT` | The person did not design, implement, or maintain the candidate, and chose their own normal work during the observation period. | Yes, subject to the approved evidence minimum |
| `CONTRIBUTOR` | The person contributed code, documentation, release work, or candidate design. | No; retain separately as supporting field context |
| `DIRECTED_TEST` | The activity followed a scripted project test rather than self-directed daily work. | No; classify as manual or compatibility evidence |
| `AUTOMATED` | No human used the candidate as a terminal for the reported activity. | No |

Filing a defect discovered during ordinary use does not by itself make someone
a contributor. Independence is self-declared; it must not be inferred from a
real name, public account, employer, location, or other personal information.

One person counts once in the cohort even when they use several machines or
platforms. Each distinct platform and artifact still receives its own
environment record. Duplicate submissions are linked by the campaign-local
code and retained rather than counted as additional people.

## Supported-platform coverage

Evidence is collected and reported separately for every shipped platform:

| Platform | Required platform-specific context |
| --- | --- |
| Linux | Distribution/version class, architecture, Wayland or X11, desktop/compositor class, packaging type, GPU/backend class, common shells, and display-scale class |
| macOS | macOS major version, Apple Silicon class, artifact type, Metal adapter class, Retina/external-display class, common shells, and window lifecycle |
| Windows | Windows edition/version/build class, x86_64 artifact type, ConPTY shells, GPU/backend class, display-scale class, native lifecycle, clipboard/IME, Unicode paths/environment, and unsigned-binary first run |

Linux results do not substitute for Windows or macOS. macOS results do not
substitute for Windows. A successful `windows-latest` automated run is not
external Windows daily use. The approved per-platform cohort floor must be met
independently; a larger Linux cohort cannot fill a Windows gap.

Windows reports must distinguish PowerShell 7, Windows PowerShell 5.1, and
`cmd.exe` where used. They also retain current Windows limitations, including
fresh-shell workspace restore and the absence of Unix detached-session hosting,
rather than treating those limitations as Linux-equivalent passes.

## Candidate identity and observation window

All qualifying records in one campaign refer to the approved candidate:

- exact OdyTTY version and full commit SHA;
- artifact filename, packaging route, byte size, and SHA-256 digest;
- commit shown by About or Copy diagnostics;
- whether the artifact was an official release artifact or a clean
  release-profile source build; and
- first and last UTC calendar dates of use.

An identity mismatch disqualifies the entry from the candidate cohort but does
not erase a defect that may affect another build. A dirty build is recorded as
non-qualifying context.

The observation window starts only after all approved candidate artifacts are
available. A candidate change that can affect observed behavior restarts the
period, as required by the stabilization policy. The approved replacement rule
must state how documentation-only changes, packaging-only changes, and
emergency replacements are handled. No restart decision is made after seeing
whether the existing results are favorable.

## Public-safe environment record

Record broad, reproducible classes rather than machine identity:

| Field | Public-safe entry |
| --- | --- |
| Campaign code | Approved campaign identifier |
| Participant code | Campaign-local synthetic code |
| Relationship | `INDEPENDENT`, `CONTRIBUTOR`, `DIRECTED_TEST`, or `AUTOMATED` |
| Platform | Linux, macOS, or Windows |
| OS class | Public distribution/version, macOS major version, or Windows edition/version/build |
| Architecture | For example `x86_64` or `aarch64` |
| Artifact identity | Filename, packaging route, size, SHA-256 digest, version, and full commit |
| Display class | Display count, resolution/refresh class, and scale class |
| Window stack | Desktop/compositor and Wayland/X11 class, macOS window server, or Windows desktop compositor |
| Hardware class | Laptop/desktop/virtual-machine class, CPU architecture class, and RAM range |
| GPU class | Vendor/product family, driver public version, backend, and hardware/software classification |
| Input class | Keyboard layout family, pointer/touchpad class, and IME family |
| Shell mix | Shell names and public versions |
| Application mix | Public application names and versions when safe, otherwise workflow categories |
| Configuration class | Defaults, plain profile, effects-on, reduced-motion, or repository-relative sanitized configuration |
| Started and completed | UTC calendar dates |
| Consent | `YES` and UTC date |
| Limitations | Missing hardware, workflows, applications, or platform paths |

Do not record a username, real name, email address, hostname, device serial,
hardware UUID, network address, exact location, employer, private host alias,
personal filesystem path, full environment dump, shell history, command line,
document title, repository name from private work, clipboard content, SSH
destination, credential, token, or terminal output.

## Daily-use observation

Create one entry for each day on which the candidate was used. Empty days are
not backfilled. The approved minimum determines whether the complete record
qualifies.

| Field | Required entry |
| --- | --- |
| Day index and UTC date | Sequential campaign day and date |
| Approximate active-use duration | Approved duration value or band |
| Start/stop count | Approximate number of application launches and ordinary exits |
| Workflow categories | Shell commands, editor, pager, multiplexer, full-screen TUI, build/test, remote SSH, tabs/panes/workspaces, graphics, or other public-safe category |
| Shells and public applications | Names and versions only when their disclosure is safe |
| Lifecycle exercised | Resize, scale change, minimize/restore, suspend/resume, disconnect/reconnect, long idle, or none |
| Input exercised | Keyboard, pointer, wheel/touchpad, clipboard, IME, mouse protocol, or accessibility alternative |
| Presentation path | Plain/effects-off, effects-on, reduced motion, or mixed |
| Stability | Crash, hang, forced termination, or clean use counts |
| Compatibility | Issue codes for incorrect application behavior, or `NONE OBSERVED` |
| Resource behavior | Stable, suspected growth, confirmed growth, or not observed; include only sanitized measurement references |
| Platform behavior | Issue codes for native-window, GPU, clipboard, input, path, process, or lifecycle defects |
| Recovery | Whether work continued, a restart was needed, state was lost, or the system required recovery |
| New issue references | Public issue numbers or security-report references |
| Notes | Concise public-safe observations and limitations |

`NONE OBSERVED` means no problem was noticed in the exercised scope. It does
not assert that the feature or platform is defect-free. A day containing a
failure still counts as observed use and must not be removed from the duration
or cohort totals.

### Copyable daily entry template

```text
Daily entry: <CAMPAIGN-CODE>/<PARTICIPANT-CODE>/<DAY-INDEX>
UTC date: <YYYY-MM-DD>
Candidate commit: <FULL-SHA>
Artifact SHA-256: <DIGEST>
Approximate active-use duration: <APPROVED VALUE OR BAND>
Workflow categories: <PUBLIC-SAFE LIST>
Shells/applications: <PUBLIC-SAFE LIST OR WITHHELD>
Lifecycle exercised: <LIST OR NONE>
Input exercised: <LIST>
Presentation path: <PLAIN / EFFECTS / REDUCED MOTION / MIXED>
Stability: <COUNTS>
Compatibility: <ISSUE CODES OR NONE OBSERVED>
Resource behavior: <STATE AND SANITIZED REFERENCE>
Platform behavior: <ISSUE CODES OR NONE OBSERVED>
Recovery: <OUTCOME>
New issue references: <PUBLIC NUMBERS OR SECURITY REFERENCES>
Limitations: <PUBLIC-SAFE TEXT>
```

This template is illustrative and contains no observation.

## Synthetic evidence and sanitization

A field observation may originate in real work, but its published reproduction
must use synthetic content. Replace private commands, paths, hostnames, prompts,
titles, clipboard values, images, and document contents with minimal fabricated
fixtures that preserve only the failure mechanism.

Acceptable evidence includes:

- a minimal synthetic byte transcript or terminal command;
- a repository-owned public fixture;
- a concise redrawn diagram of geometry or lifecycle order;
- a bounded, manually sanitized diagnostic excerpt;
- broad before/after resource measurements with the collection method; and
- a public issue number containing the sanitized reproduction.

Do not publish raw logs, complete diagnostic dumps, memory dumps, shell history,
real terminal captures, personal screen captures, private configuration,
private URLs, real SSH destinations, credentials, or crash artifacts that have
not been inspected and sanitized. Operating-system error strings may contain
paths and require the same inspection.

Sanitization must not alter the technical failure. Record every replacement or
omission that could affect reproduction as a limitation. If useful evidence
cannot be made public safely, route it through the security-reporting path and
publish only a non-sensitive reference and classification.

## Issue taxonomy

Every unfavorable observation receives one primary category:

| Code | Category | Examples |
| --- | --- | --- |
| `CRASH` | Crash or abort | Process abort, panic termination, native crash |
| `HANG` | Hang or freeze | Unresponsive window, stuck shutdown, stalled output |
| `LOSS` | Data or state loss | Lost terminal state, incorrect restore, destructive clipboard or input behavior |
| `SECURITY` | Security or privacy | Boundary bypass, unintended disclosure, unsafe opener or transport behavior |
| `COMPAT` | Application compatibility | Incorrect terminal semantics, TUI corruption, unsupported sequence needed by an exercised application |
| `INPUT` | Keyboard, IME, or mouse | Missing, duplicated, delayed, or incorrectly routed input |
| `RENDER` | Text, graphics, or presentation | Glyph defects, clipping, image placement, unreadable effect |
| `CLIPBOARD` | Selection, clipboard, or open action | Copy/paste corruption, consent error, wrong target, unintended opener |
| `PTY` | Shell, PTY, or process lifecycle | Spawn, resize, exit, signal, ConPTY, or child-containment defect |
| `LAYOUT` | Tab, pane, workspace, or restore | Focus, split, close escalation, persistence, or geometry defect |
| `SESSION` | Detached session, SSH, or integration | Attach, reconnect, remote integration, or session lifecycle defect |
| `RESOURCE` | CPU, memory, GPU memory, or wake growth | Confirmed or suspected unbounded growth, idle activity, resource exhaustion |
| `WINDOW` | GPU, display, or native lifecycle | Adapter, surface, scale, compositor, minimize, suspend, restore, or first-run defect |
| `ACCESS` | Accessibility alternative | Reduced motion, contrast, keyboard reachability, focus, or visual bell defect |
| `INSTALL` | Artifact, installation, or documentation | Package failure, provenance mismatch, misleading or missing instructions |

Assign one severity based on observed impact, independently of release policy:

| Severity | Observed impact |
| --- | --- |
| `S1` | Security exposure, data loss, unsafe process behavior, or system-level recovery required |
| `S2` | Crash, persistent hang, unusable core terminal path, or repeated loss of work with no reasonable workaround |
| `S3` | Material compatibility or platform failure with a usable workaround |
| `S4` | Limited presentation, usability, installation, or documentation defect |

Severity describes the observation. The mapping from severity, frequency,
reproducibility, and platform scope to a release block remains the unset
approval decision above.

Each issue also records:

- affected platform and candidate identity;
- first-observed and reported UTC dates;
- occurrence count and approximate use duration;
- reproduction state: `UNTRIED`, `NOT REPRODUCED`, `INTERMITTENT`,
  `DETERMINISTIC`, `FIXED UNCONFIRMED`, or `FIXED CONFIRMED`;
- first deterministic reproduction date, when achieved;
- time from report to deterministic reproduction, or `NOT REPRODUCED`;
- recovery and work-loss impact;
- sanitized reproduction and evidence references;
- known workaround;
- current disposition and any bounded-exception reference; and
- limitations.

### Copyable issue entry template

```text
Issue code: <CAMPAIGN-CODE>-<SEQUENCE>
Primary category: <TAXONOMY CODE>
Severity: <S1 / S2 / S3 / S4>
Candidate commit: <FULL-SHA>
Artifact SHA-256: <DIGEST>
Platform class: <PUBLIC-SAFE CLASS>
First observed: <YYYY-MM-DD>
Reported: <YYYY-MM-DD>
Occurrence count: <COUNT>
Approximate use before occurrence: <VALUE OR BAND>
Reproduction state: <STATE>
First deterministic reproduction: <YYYY-MM-DD OR NOT REPRODUCED>
Time to reproduce: <DURATION OR NOT REPRODUCED>
Recovery and work loss: <PUBLIC-SAFE SUMMARY>
Synthetic reproduction: <STEPS OR REFERENCE>
Evidence: <PUBLIC NUMBER OR SECURITY REFERENCE>
Workaround: <TEXT OR NONE>
Disposition: <OPEN / FIXED / DOCUMENTED LIMITATION / EXCEPTION CANDIDATE>
Limitations: <PUBLIC-SAFE TEXT>
```

This template is illustrative and contains no issue.

## Withdrawal

Participation may stop without explanation. No future entry is expected after
withdrawal.

Before publication, a draft may be deleted. After publication, the canonical
record may be redacted or removed when needed for privacy or safety, but copies
outside the canonical repository cannot be recalled. The withdrawal date and
the fact that a record ceased qualifying are retained without a personal
identifier.

Withdrawal must not silently convert an unfavorable observation into a
favorable one. Subject to the approved withdrawal treatment, the final
aggregate retains the sanitized fact that an issue occurred, its platform,
category, severity, and disposition while removing participant-linked context
that is no longer appropriate to retain.

## Evidence retention and corrections

- Favorable and unfavorable qualifying records use the same retention rule.
- Fixed, duplicate, intermittent, and not-reproduced issues remain in the
  campaign history with their final disposition.
- Corrections append a dated correction note; they do not overwrite the
  original observation without trace.
- Missing days, skips, withdrawals, disqualified builds, and platform gaps are
  counted and disclosed.
- A failure found late in the period is not excluded because there is
  insufficient time to fix it.
- Evidence gathered after the approved window is labeled separately and cannot
  be inserted retroactively to satisfy the minimum.
- Security-sensitive details may remain non-public, but the sanitized category,
  severity, platform scope, status, and reference remain visible.

## Final field report contract

After the approved observation period ends, publish a sanitized
`docs/field-report.md` containing:

1. The approved program values and any changes made before collection began.
2. Candidate version, full commit, artifact names, and SHA-256 digests.
3. Actual start and end dates, including any restart and its reason.
4. Total people and qualifying independent people, separated by Linux, macOS,
   and Windows without double-counting multi-platform individuals.
5. Evidence-minimum completion, skips, withdrawals, disqualified records, and
   platform gaps.
6. Aggregated use days, duration values or bands, workflow categories,
   lifecycle paths, input paths, presentation paths, shells, and public
   applications.
7. Every issue grouped by platform, category, severity, reproduction state,
   occurrence count, recovery impact, and disposition.
8. Crash, hang, compatibility, platform, and confirmed or suspected
   resource-growth observations, including zero counts only when the required
   evidence was present.
9. Time-to-reproduce results and unresolved reproduction gaps.
10. Every unfavorable result, fixed result, documented limitation, security
    reference, and proposed bounded exception.
11. The accepted release-blocking threshold, its mechanical application, and
    any exception with mitigation and expiry.
12. Limitations and a conclusion that states whether the approved evidence
    contract was met without expanding the claim beyond the observed cohort.

Do not publish a composite score, satisfaction percentage, or unsupported
daily-driver claim. The report distinguishes `none observed` from `proven
absent`, and it does not treat a small voluntary cohort as representative of
all terminal users.

## Program sequence

The program may proceed only in this order:

1. Fill and approve every decision value.
2. Freeze the exact candidate and public artifact identities.
3. Publish the consent text, blank templates, platform requirements, and issue
   taxonomy before any observation is accepted.
4. Open voluntary enrollment without adding product instrumentation.
5. Collect deliberate human-written entries for the complete approved period.
6. Sanitize and classify entries without suppressing unfavorable results.
7. Close the window at the preregistered time; do not extend it solely to
   improve the outcome.
8. Publish the field report and unresolved gaps.
9. Apply the preregistered release threshold and exception policy.

This draft completes none of those steps. It defines the material that must be
approved before they can begin.
