#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# RustSec policy gate shared by the PR, scheduled, and release-tag workflows.
set -euo pipefail

exception_expiry=2026-08-15
today="$(date -u +%F)"
if [[ "$today" > "$exception_expiry" ]]; then
  echo "::error::quick-xml exceptions expired on ${exception_expiry}; remove or renew them" >&2
  exit 1
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
