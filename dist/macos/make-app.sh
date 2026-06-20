#!/usr/bin/env bash
# Assemble OdyTTY.app from a pre-built universal `odytty` binary.
#
# Usage: bash dist/macos/make-app.sh <version>
# Expects: dist/build/odytty            (the `odytty` binary from a local
#                                        `cargo build --release`; see the macOS
#                                        install steps in README.md)
#          dist/macos/odytty-1024.png   (icon source, committed)
#          dist/macos/Info.plist        (manifest template with __VERSION__)
# Produces: dist/build/OdyTTY.app
#
# macOS-only: uses `sips` and `iconutil` (system tools) to build the .icns.
set -euo pipefail

VERSION="${1:?usage: make-app.sh <version>}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BUILD="$ROOT/dist/build"
APP="$BUILD/OdyTTY.app"
BIN="$BUILD/odytty"
ICON_SRC="$ROOT/dist/macos/odytty-1024.png"
PLIST_SRC="$ROOT/dist/macos/Info.plist"

[ -f "$BIN" ] || { echo "missing universal binary: $BIN" >&2; exit 1; }
[ -f "$ICON_SRC" ] || { echo "missing icon source: $ICON_SRC" >&2; exit 1; }

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

# Binary
cp "$BIN" "$APP/Contents/MacOS/odytty"
chmod +x "$APP/Contents/MacOS/odytty"

# Icon: build a proper multi-resolution .icns from the 1024px source.
ICONSET="$BUILD/odytty.iconset"
rm -rf "$ICONSET"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$ICON_SRC" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
  d=$((size * 2))
  sips -z "$d" "$d" "$ICON_SRC" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/odytty.icns"
rm -rf "$ICONSET"

# Manifest: substitute the version into the committed template.
sed "s/__VERSION__/${VERSION}/g" "$PLIST_SRC" > "$APP/Contents/Info.plist"

echo "Built $APP (version $VERSION)"
