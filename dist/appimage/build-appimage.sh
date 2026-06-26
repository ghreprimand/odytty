#!/usr/bin/env bash
# Build an x86_64 AppImage for OdyTTY.
#
# Usage:
#   dist/appimage/build-appimage.sh [version]
#
# If [version] is omitted it is read from Cargo.toml. The release binary is
# built if target/release/odytty is missing. The finished AppImage is written
# to the repository root as odytty-<version>-x86_64.AppImage.
#
# Tooling: linuxdeploy (plus its appimage output plugin) does the dependency
# bundling. linuxdeploy ships a default exclude list that deliberately leaves
# the graphics stack on the host — libvulkan, libGL, the X11/Wayland client
# libs, and glibc are NOT bundled — so the AppImage uses the host Mesa/Vulkan
# ICD rather than carrying a driver that would mismatch the user's GPU. That is
# the documented AppImage caveat: the host must provide a working Vulkan driver.
#
# No FUSE is required: APPIMAGE_EXTRACT_AND_RUN=1 makes both linuxdeploy and the
# nested appimagetool self-extract instead of mounting, which is what CI needs.
#
# Local overrides (skip the downloads):
#   LINUXDEPLOY=/path/to/linuxdeploy-x86_64.AppImage
#   LINUXDEPLOY_PLUGIN_APPIMAGE=/path/to/linuxdeploy-plugin-appimage-x86_64.AppImage
set -euo pipefail

ARCH=x86_64
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

VERSION="${1:-$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)}"
if [ -z "$VERSION" ]; then
  echo "error: could not determine version" >&2
  exit 1
fi
echo "Building OdyTTY AppImage v$VERSION ($ARCH)"

BIN=target/release/odytty
if [ ! -x "$BIN" ]; then
  echo "==> building release binary"
  cargo build --release --locked
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
APPDIR="$WORK/AppDir"
mkdir -p "$APPDIR/usr/share/metainfo" "$APPDIR/usr/share/icons/hicolor"

# Pre-seed AppDir with AppStream metadata and the full hicolor icon set so the
# AppImage carries proper desktop integration metadata, not just one icon.
cp dist/linux/io.unfinished_works.odytty.metainfo.xml \
  "$APPDIR/usr/share/metainfo/"
cp -a dist/icons/hicolor/. "$APPDIR/usr/share/icons/hicolor/"

# Fetch tooling (or use caller-provided paths).
fetch() { # url dest
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL -o "$2" "$1"
  else
    wget -qO "$2" "$1"
  fi
  chmod +x "$2"
}

LD="${LINUXDEPLOY:-$WORK/linuxdeploy-$ARCH.AppImage}"
if [ ! -x "$LD" ] || [ -z "${LINUXDEPLOY:-}" ]; then
  echo "==> downloading linuxdeploy"
  fetch "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-$ARCH.AppImage" "$LD"
fi

PLUGIN="${LINUXDEPLOY_PLUGIN_APPIMAGE:-$WORK/linuxdeploy-plugin-appimage-$ARCH.AppImage}"
if [ ! -x "$PLUGIN" ] || [ -z "${LINUXDEPLOY_PLUGIN_APPIMAGE:-}" ]; then
  echo "==> downloading linuxdeploy appimage plugin"
  fetch "https://github.com/linuxdeploy/linuxdeploy-plugin-appimage/releases/download/continuous/linuxdeploy-plugin-appimage-$ARCH.AppImage" "$PLUGIN"
fi
# The appimage output plugin must be discoverable on PATH by linuxdeploy.
ln -sf "$PLUGIN" "$WORK/linuxdeploy-plugin-appimage"
export PATH="$WORK:$PATH"

export APPIMAGE_EXTRACT_AND_RUN=1
export VERSION
export OUTPUT="odytty-$VERSION-$ARCH.AppImage"

echo "==> bundling with linuxdeploy"
"$LD" --appimage-extract-and-run \
  --appdir "$APPDIR" \
  --executable "$BIN" \
  --desktop-file dist/linux/io.unfinished_works.odytty.desktop \
  --icon-file dist/icons/hicolor/256x256/apps/io.unfinished_works.odytty.png \
  --output appimage

# linuxdeploy writes OUTPUT into the cwd (repo root).
if [ ! -f "$OUTPUT" ]; then
  echo "error: expected $OUTPUT was not produced" >&2
  exit 1
fi
chmod +x "$OUTPUT"
echo "==> built $OUTPUT"
ls -lh "$OUTPUT"
