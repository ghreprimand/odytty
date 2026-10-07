#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# RustSec policy gate shared by the PR, scheduled, and release-tag workflows.
set -euo pipefail

# The release-tag workflow passes a context argument. No policy varies by
# context any more, so the argument is accepted and ignored rather than
# rejected; the same rules apply to pull-request, scheduled, and tag runs.

# No advisory is ignored. The removed ttf-parser dependency remains forbidden
# even when cargo-audit classifies its advisory as informational.
lockfiles=(Cargo.lock fuzz/parser_graphics/Cargo.lock)
for lockfile in "${lockfiles[@]}"; do
  if [[ ! -f "$lockfile" ]]; then
    echo "RustSec: required lockfile missing: $lockfile" >&2
    exit 1
  fi
  if grep -q '^name = "ttf-parser"$' "$lockfile"; then
    echo "RustSec: forbidden ttf-parser dependency returned in $lockfile" >&2
    exit 1
  fi
done
cargo audit --version
set +e
audit_output=""
audit_status=0
for lockfile in "${lockfiles[@]}"; do
  result="$(cargo audit --deny unsound --file "$lockfile" 2>&1)"
  status=$?
  audit_output+="== $lockfile =="$'\n'"$result"$'\n'
  if (( status != 0 )); then
    audit_status=$status
  fi
done
set -e
printf '%s' "$audit_output"
if (( audit_status != 0 )); then
  exit "$audit_status"
fi

# cargo-audit keeps its clone under CARGO_HOME. Preserve the scanner and
# advisory-database identity in release logs for incident reconstruction.
advisory_db="${CARGO_HOME:-$HOME/.cargo}/advisory-db"
if git -C "$advisory_db" rev-parse HEAD >/dev/null 2>&1; then
  echo "RustSec advisory database: $(git -C "$advisory_db" rev-parse HEAD)"
else
  echo "::warning::RustSec advisory database revision unavailable" >&2
fi
