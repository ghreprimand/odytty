#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Validate the release version before writing workflow records or filenames.
set -euo pipefail
if [ "$#" -ne 0 ]; then
  echo 'usage: derive-version.sh' >&2
  exit 2
fi
if [ "${GITHUB_REF_TYPE:-}" = tag ]; then
  if [[ "${GITHUB_REF_NAME:-}" != v* ]]; then
    echo 'invalid release tag: expected vX.Y.Z' >&2
    exit 1
  fi
  version="${GITHUB_REF_NAME#v}"
else
  version="$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml | head -n 1)"
fi
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo 'invalid release version: expected X.Y.Z' >&2
  exit 1
fi
printf '%s\n' "$version"
