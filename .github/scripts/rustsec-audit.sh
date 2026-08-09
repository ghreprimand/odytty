#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# RustSec policy gate shared by the PR, scheduled, and release-tag workflows.
set -euo pipefail

# The release-tag workflow passes a context argument. No policy varies by
# context any more, so the argument is accepted and ignored rather than
# rejected; the same rules apply to pull-request, scheduled, and tag runs.

# No advisory is ignored. RUSTSEC-2026-0194 and RUSTSEC-2026-0195 were
# suppressed while wayland-scanner pinned quick-xml ^0.39; wayland-scanner
# 0.31.11 takes quick-xml 0.41.0, which carries both fixes, so the
# suppressions and their expiry fuse were removed on 2026-07-30. A downgrade
# back to an affected quick-xml now fails this gate on its own, which is why
# no separate dependency-graph assertion is kept here.
cargo audit --version

# RUSTSEC-2026-0192 is an informational unmaintained warning, not a vulnerability
# or unsoundness advisory, so cargo-audit exits zero under the policy above. Its
# bounded exception expires on 2026-10-15. The fuse is conditional on the exact
# advisory remaining in the scan: an upstream release or RustSec withdrawal
# clears it without a script edit, while carrying it through the deadline makes
# every scheduled, pull-request, and release audit fail closed.
set +e
audit_output="$(cargo audit --deny unsound 2>&1)"
audit_status=$?
set -e
printf '%s\n' "$audit_output"
if (( audit_status != 0 )); then
  exit "$audit_status"
fi

ttf_parser_advisory="RUSTSEC-2026-0192"
ttf_parser_expiry="2026-10-15"
audit_date="${ODYTTY_AUDIT_DATE:-$(date -u +%F)}"
if [[ ! "$audit_date" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]]; then
  echo "invalid audit date: $audit_date" >&2
  exit 2
fi
if grep -q "$ttf_parser_advisory" <<<"$audit_output" \
  && [[ "$audit_date" > "$ttf_parser_expiry" || "$audit_date" == "$ttf_parser_expiry" ]]; then
  echo "$ttf_parser_advisory exception expired on $ttf_parser_expiry" >&2
  exit 1
fi

# cargo-audit keeps its clone under CARGO_HOME. Preserve the scanner and
# advisory-database identity in release logs for incident reconstruction.
advisory_db="${CARGO_HOME:-$HOME/.cargo}/advisory-db"
if git -C "$advisory_db" rev-parse HEAD >/dev/null 2>&1; then
  echo "RustSec advisory database: $(git -C "$advisory_db" rev-parse HEAD)"
else
  echo "::warning::RustSec advisory database revision unavailable" >&2
fi
