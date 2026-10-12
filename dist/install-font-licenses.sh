#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Copy the licence texts of the bundled fonts into a package tree.
#
# usage: install-font-licenses.sh DEST
#
# The fonts are embedded in the binary, and the SIL Open Font License and the
# other font licences require their notices to travel with every copy. Each
# path listed in dist/font-licenses.txt lands at DEST/<font set>/<file>. The
# list is the one place that names the files; the tarball, the AUR package,
# the AppImage and the macOS app call this script, and the Windows zip steps
# read the list in PowerShell. The deb and rpm lists in Cargo.toml name the
# same files, and tests/packaging_font_licenses.rs keeps every consumer in step
# with the font directory.
set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "usage: $0 DEST" >&2
    exit 2
fi
dest=$1
root="$(cd "$(dirname "$0")/.." && pwd)"

while IFS= read -r path || [ -n "$path" ]; do
    [ -n "$path" ] || continue
    relative="${path#assets/fonts/}"
    if [ "$relative" = "$path" ] || [ ! -f "$root/$path" ]; then
        echo "install-font-licenses: not a bundled font licence file: $path" >&2
        exit 1
    fi
    mkdir -p "$dest/$(dirname "$relative")"
    cp "$root/$path" "$dest/$relative"
    chmod 0644 "$dest/$relative"
done < "$root/dist/font-licenses.txt"
