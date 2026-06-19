#!/usr/bin/env bash
# Package OdyTTY.app into a distributable .dmg with an /Applications symlink
# for drag-to-install.
#
# Usage: bash dist/macos/make-dmg.sh <version>
# Expects: dist/build/OdyTTY.app (built by make-app.sh)
# Produces: dist/build/odytty-<version>-macos-universal.dmg
#
# macOS-only: uses `hdiutil`.
set -euo pipefail

VERSION="${1:?usage: make-dmg.sh <version>}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BUILD="$ROOT/dist/build"
APP="$BUILD/OdyTTY.app"
DMG="$BUILD/odytty-${VERSION}-macos-universal.dmg"
STAGING="$BUILD/dmg-staging"

[ -d "$APP" ] || { echo "missing app bundle: $APP" >&2; exit 1; }

rm -rf "$STAGING" "$DMG"
mkdir -p "$STAGING"
cp -R "$APP" "$STAGING/"
ln -s /Applications "$STAGING/Applications"

hdiutil create \
  -volname "OdyTTY $VERSION" \
  -srcfolder "$STAGING" \
  -fs HFS+ \
  -format UDZO \
  -ov \
  "$DMG"

rm -rf "$STAGING"
echo "Built $DMG"
