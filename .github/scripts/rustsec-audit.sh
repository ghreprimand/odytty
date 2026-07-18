#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# RustSec policy gate shared by the PR, scheduled, and release-tag workflows.
set -euo pipefail

# quick-xml advisories RUSTSEC-2026-0194/-0195 are fixed in quick-xml 0.41.0,
# but wayland-scanner 0.31.10 (the sole dependent, a compile-time proc macro)
# pins ^0.39, and no upstream release takes 0.41 yet. Removal trigger: a
# `cargo update` resolves this the moment a wayland-scanner 0.31.x ships with
# quick-xml >= 0.41 -- at which point drop the --ignore flags and this fuse.
exception_expiry=2026-10-15
# Fourteen-day early-warning window: PR/scheduled runs fail once inside it so a
# renewal is prompted well before a tag push could be ambushed; release-tag runs
# still pass until the hard expiry. Dates are UTC %F strings, so a lexicographic
# compare is also a chronological one.
warn_start="$(date -u -d "${exception_expiry} - 14 days" +%F)"
today="$(date -u +%F)"

# Release context is passed explicitly by the release-tag workflow so the
# early-warning window can pass there while still failing PR/scheduled runs.
release_context=0
if [[ "${1:-}" == "--release" ]]; then
  release_context=1
fi

if [[ "$today" > "$exception_expiry" ]]; then
  echo "::error::quick-xml exceptions expired on ${exception_expiry}; remove or renew them" >&2
  exit 1
elif [[ ! "$today" < "$warn_start" ]]; then
  if [[ "$release_context" -eq 1 ]]; then
    echo "::warning::quick-xml exceptions expire on ${exception_expiry} (within 14 days); renew the expiry or upgrade wayland-scanner past quick-xml 0.41 soon. Release builds still pass until expiry." >&2
  else
    echo "::error::quick-xml exceptions expire on ${exception_expiry} (within 14 days); renew the expiry or upgrade wayland-scanner past quick-xml 0.41. This early warning fails PR and scheduled runs 14 days ahead so a tag push is never ambushed." >&2
    exit 1
  fi
fi

# Keep the exception graph explicit. quick-xml 0.39.4 must remain the
# wayland-scanner 0.31.10 proc-macro dependency until the upstream Winit stack
# moves together.
scanner_tree="$(cargo tree --locked -p wayland-scanner@0.31.10)"
printf '%s\n' "$scanner_tree"
grep -Fq 'quick-xml v0.39.4' <<<"$scanner_tree"
quick_xml_tree="$(cargo tree --locked -i quick-xml@0.39.4)"
printf '%s\n' "$quick_xml_tree"
grep -Fq 'wayland-scanner v0.31.10' <<<"$quick_xml_tree"

# RUSTSEC-2026-0194: duplicate-attribute quadratic work is reachable only in
# wayland-scanner's compile-time proc macro. Its protocol XML comes from
# checksum-verified Cargo dependencies, never OdyTTY runtime or PTY input.
echo "RUSTSEC-2026-0194 exception: trusted compile-time wayland-scanner XML; expires ${exception_expiry}"
# RUSTSEC-2026-0195: NsReader namespace allocation is not linked into OdyTTY;
# the same trusted compile-time wayland-scanner path is the complete graph.
echo "RUSTSEC-2026-0195 exception: no OdyTTY runtime NsReader input; expires ${exception_expiry}"

cargo audit --version
cargo audit \
  --deny unsound \
  --ignore RUSTSEC-2026-0194 \
  --ignore RUSTSEC-2026-0195

# cargo-audit keeps its clone under CARGO_HOME. Preserve the scanner and
# advisory-database identity in release logs for incident reconstruction.
advisory_db="${CARGO_HOME:-$HOME/.cargo}/advisory-db"
if git -C "$advisory_db" rev-parse HEAD >/dev/null 2>&1; then
  echo "RustSec advisory database: $(git -C "$advisory_db" rev-parse HEAD)"
else
  echo "::warning::RustSec advisory database revision unavailable" >&2
fi
