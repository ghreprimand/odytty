# Temporary Stabilization and Release-Channel Policy

This policy governs OdyTTY development and releases while the pre-1.0
stabilization program is active. It remains in force until a tracked replacement
states that the program has ended.

The purpose of the freeze is to turn current behavior into reproducible
compatibility, performance, security, platform, and field evidence. Version
progression and feature count are not evidence of readiness.

## Scope Freeze

A change qualifies for the stabilization branch of work only when its primary
purpose is one of the following:

- a behavior-preserving refactor that makes correctness easier to inspect or
  test;
- a compatibility, correctness, security, or resource-use fix;
- a focused test, fuzz target, benchmark harness, or evidence report;
- dependency and release-pipeline hardening;
- documentation that reconciles public claims with current evidence; or
- a defect fix already accepted into the stabilization program.

New profiles, graphics protocols, themes, workflow layers, dashboards, plugin
or AI systems, unrelated visual polish, and new default behaviors do not
qualify. An urgent fix must not carry opportunistic features or broad cleanup.
Any exception to this boundary must be explicit, bounded, and recorded before
implementation.

Every qualifying change must preserve the declared Rust version lockstep, pass
the repository's required local checks, and retain the blocking Linux, macOS,
and Windows CI matrix. Behavior-preserving work must keep public APIs, command
output, defaults, golden data, and terminal semantics unchanged unless the
change is specifically a behavior fix.

## Release Channels

OdyTTY has three release states during stabilization:

| State | Purpose | Expectation |
| --- | --- | --- |
| Mainline | Integrates qualified stabilization work | May contain changes not yet included in a published build; every change must pass the normal merge gates |
| Pre-1.0 release | Publishes a versioned build for compatibility and field evidence | Suitable for evaluation, but still subject to documented gaps and pre-1.0 change risk |
| Emergency release | Replaces an affected published build with a narrow correctness or security fix | Minimal scope, expedited timing, and the same mandatory build and test gates |

Manual release-workflow runs are artifact validation, not a published channel.
Only a version tag publishes a release. The stable 1.0 channel does not exist
until the pre-1.0 acceptance contract is satisfied and a 1.0 release is
explicitly approved.

Always-latest download aliases follow the newest published release. Consumers
that require reproducibility should use a version-pinned artifact and verify its
checksum.

## Routine Pre-Release Cadence

Routine pre-1.0 releases target a minimum fourteen-day interval. A release is
cut only when the accumulated changes provide a coherent evidence or
correctness increment; elapsed time alone does not require a release.

Each routine build collects at least fourteen consecutive days of
telemetry-free field evidence before a planned successor is published. The
period starts when all intended artifacts are available. It records public-safe
reports of crashes, compatibility failures, resource growth, platform defects,
and unresolved release blockers. A replacement candidate restarts the period
when it changes behavior relevant to the evidence being collected.

The fourteen-day period is a cadence floor, not a 1.0 readiness shortcut. The
separate external observation program may require a longer period, broader
platform coverage, or more independent reports.

A routine release waits for:

- the complete local gate on the exact candidate revision;
- successful blocking CI on Linux, macOS, and Windows for that revision;
- completed release-profile validation for every platform-sensitive surface
  changed since the previous release;
- reconciled documentation and known limitations; and
- no unresolved release-blocking correctness or security failure.

## Emergency Correctness and Security Releases

An emergency release may ship before the fourteen-day interval or observation
period ends when delaying a confirmed high-impact correctness or security fix
would expose users to greater risk. It contains the smallest supportable fix,
focused regression coverage, necessary dependency changes, and accurate public
guidance.

Emergency timing does not waive formatting, lint, test, audit, artifact, or
blocking platform gates. Platform impact must be assessed explicitly. When the
affected surface is Windows-sensitive, native Windows validation is required in
addition to the unchanged blocking Windows CI leg.

After publication, the emergency build becomes the evidence candidate and
starts a new observation period. Any broader refactor or follow-up improvement
returns to the routine cadence.

## Windows Policy

Windows remains a first-class supported and shipped target throughout
stabilization. The `windows-latest` job remains a blocking regression gate and
must not be removed, made advisory, or narrowed to admit a release.

Changes involving PTY behavior, process spawning, paths, environment handling,
logging, shell integration, state or session files, URLs, transports, input,
native windows, or GPU behavior require an explicit Windows impact statement.
If a change can affect Windows behavior, it also requires Windows-specific
automated coverage where practical and release-profile validation on Windows.
Passing on Unix is not evidence of Windows correctness, and Unix-only reasoning
must not be used to close a Windows validation item.

## Version Semantics

Every `0.y.z` release remains pre-1.0. Patch and minor version changes communicate
release identity; they do not complete an acceptance gate or promise automatic
promotion to 1.0.

In particular, `0.9.x` does not automatically advance to `1.0.0`.
`0.10.0` is a valid newer pre-release, followed by further `0.y.z` versions as
needed. A 1.0 version is reserved for the explicit release decision made from
the complete, current evidence bundle and any recorded bounded exceptions.

The active stabilization program targets `v0.10.0` as its next release
decision. That decision is governed by the narrowed gate scope defined in the
pre-1.0 acceptance contract's current-release-decision section: `v0.10.0`
remains a pre-1.0 release, shipping it does not complete the acceptance
contract, and the deferred programs named there do not block it.
