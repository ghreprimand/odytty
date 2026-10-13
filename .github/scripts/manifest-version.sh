#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Print the version from the [package] table of Cargo.toml in the current
# directory. Only that table counts: a dependency table before it can carry
# its own `version = ` line, which a plain first-match search would return.
set -euo pipefail
if [ "$#" -ne 0 ]; then
  echo 'usage: manifest-version.sh' >&2
  exit 2
fi
sed -n '/^\[package\]$/,/^\[/{s/^version = "\([^"]*\)"$/\1/p;}' Cargo.toml | head -n 1
