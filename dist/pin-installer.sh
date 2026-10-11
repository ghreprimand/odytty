#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Stamp the version-pinned installer release asset.
#
# usage: pin-installer.sh VERSION SOURCE DEST
#
# dist/install.sh downloads from releases/latest/download, so it installs
# whichever release is newest. The release asset odytty-VERSION-install.sh is
# described as version-pinned, so its BASE_URL is rewritten to that release's
# own download directory: the artifact and the signed SHA256SUMS it checks both
# come from the release the installer was published with. The rewrite is
# exact: the script fails unless SOURCE carries exactly one BASE_URL line of the
# expected shape, and DEST then carries exactly one pinned line and no "latest"
# line.
set -euo pipefail

if [ "$#" -ne 3 ]; then
    echo "usage: $0 VERSION SOURCE DEST" >&2
    exit 2
fi
version=$1
source_file=$2
dest=$3

if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "pin-installer: version must be MAJOR.MINOR.PATCH, got: $version" >&2
    exit 2
fi

# shellcheck disable=SC2016 # the line is matched literally, ${REPO} included
latest='BASE_URL="https://github.com/${REPO}/releases/latest/download"'
if [ "$(grep -cFx "$latest" "$source_file")" -ne 1 ]; then
    echo "pin-installer: $source_file must contain exactly one line: $latest" >&2
    exit 1
fi

# shellcheck disable=SC2016 # the line is written literally, ${REPO} included
pinned='BASE_URL="https://github.com/${REPO}/releases/download/v'"$version"'"'
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
while IFS= read -r line || [ -n "$line" ]; do
    if [ "$line" = "$latest" ]; then
        printf '%s\n' "$pinned"
    else
        printf '%s\n' "$line"
    fi
done < "$source_file" > "$tmp"

if [ "$(grep -cFx "$pinned" "$tmp")" -ne 1 ] || [ "$(grep -cFx "$latest" "$tmp")" -ne 0 ]; then
    echo "pin-installer: stamping failed" >&2
    exit 1
fi
cat "$tmp" > "$dest"
chmod 0755 "$dest"
