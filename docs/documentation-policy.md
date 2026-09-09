# Documentation maintenance

Documentation describes a specific source tree or evidence record. A working
branch can contain implemented but unreleased behavior; an immutable tag records
the source and documentation used for that release. The
[release index](releases/README.md) identifies the latest published version and
links corrections to old documentation.

## Publication status

The release index, README, SPEC, and TODO each carry one
`Published release: **vX.Y.Z**.` line. Update these together only after
publication is verified. A release candidate uses a separate
`Release candidate: **vX.Y.Z**.` line in all four documents; this does not claim
that the candidate has already been published.

Before tagging, the target TODO section uses `(release candidate)` and lists
completed feature and pre-publication acceptance work accurately. Outstanding
artifact and channel checks belong under `### Post-publication checks`. Deferred
work remains explicitly labelled `Deferred` and cannot conceal a required
release feature. After publication, update the candidate section to `(published)`,
record the actual artifact/channel results, update the published markers, and
remove the candidate markers in a follow-up documentation change. Do not invent
a pass to close a checkbox.

Run these offline checks when changing release-status documentation:

```sh
python3 scripts/documentation-guard-test.py
python3 scripts/documentation-guard.py
```

Release preparation also runs
`python3 scripts/documentation-guard.py --release-version X.Y.Z` and the separate
release-notes check. CI checks status convergence; the source archive and
publication jobs check the target version. The guard detects inconsistent
published markers, stale latest-release claims, unfinished target milestones,
and the regression that lists the entire shipped profile system as a gap.
It does not establish test success, verify remote publication, or replace
review of prose and retained evidence.

## Scope and evidence

- User guides describe behavior in their checked-out source tree. Unreleased
  additions are labelled with their development version. Settings and command
  examples must match the corresponding source, including platform limitations.
- Design contracts distinguish implemented behavior from remaining work. A
  described protocol is not evidence that the runtime implements it.
- Benchmarks, coverage, compatibility results, and monthly devlogs retain the
  revision and conditions of their original evidence. Old counts and recorded
  failures are historical facts; do not replace them with current results.
- Correct inaccurate historical prose with a dated correction and a link from
  current release notes. Never move a published tag or replace an archive to
  repair its README.
- Audit sibling guides, packaging/runbooks, source comments, the documentation
  index, and website handoff whenever a public claim changes. Local-only notes
  and upstream licensing material are outside the maintained product narrative.

## Release documentation review

Read README, SPEC, the target TODO milestone, release notes, installation and
packaging guides as one set before the version commit. Check known gaps against
features described as complete, defaults against settings, and status claims
against actual evidence. Check the monthly DEVLOG entry and its index link.
Preserve an explicit list of unmeasured, unsupported, failed, and deferred
items. A release requires all mandatory feature and acceptance gates even when
the documentation checks pass.

The website must use the same release notes and load every monthly DEVLOG
archive listed in the index. Repository validation and a live website check are
separate results; an unavailable website is not a successful handoff.
