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

# RUSTSEC-2026-0192 (ttf-parser unmaintained) was carried as a time-bounded
# exception while ttf-parser reached the tree directly and through
# ab_glyph. Normal-text font parsing moved to skrifa and the Wayland
# decoration backend moved to crossfont, so neither crate is in either
# lockfile and the exception block was removed on 2026-09-19. A dependency
# change that reintroduces the crate shows the informational warning in this
# output again (cargo-audit exits zero for it); treat that as a regression.
set +e
audit_output=""
audit_status=0
lockfiles=(Cargo.lock)
if [[ -f fuzz/parser_graphics/Cargo.lock ]]; then
  lockfiles+=(fuzz/parser_graphics/Cargo.lock)
fi
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
