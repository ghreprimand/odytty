# Documentation maintenance

Documentation describes a specific source tree or evidence record. A working
branch can contain implemented but unreleased behavior; an immutable tag records
the source and documentation used for that release. The
[release index](releases/README.md) identifies the latest published version and
links corrections to old documentation.

## Publication status

The release index, README, SPEC, and TODO each carry one
`Published release: **vX.Y.Z**.` line. The version commit that receives the tag
updates these together, so the tagged source already describes its own version
as published: the target TODO section uses `(published)`, any roadmap checkpoint
for the version reads as shipped, and the current devlog archive carries the
`## YYYY-MM-DD -- Release vX.Y.Z -- Summary` entry. The target TODO section lists
completed feature and acceptance work accurately; deferred work remains
explicitly labelled `Deferred` and cannot conceal a required release feature.

Artifact, signature, checksum, and package-channel verification happens after
the tag and is recorded in the devlog; it does not reopen the published
markers. If a release job fails, fix forward without moving the tag. The
documentation guard still accepts a separate `Release candidate: **vX.Y.Z**.`
line for an untagged candidate; that line does not claim publication. Do not
invent a pass to close a checkbox.

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
