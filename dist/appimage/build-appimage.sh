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
# Local overrides must be byte-identical to the pinned release assets:
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

# Verified immutable upstream release inputs. Do not substitute the mutable
# `continuous` channel: these checksums are the review boundary before either
# downloaded AppImage receives execute permission.
LINUXDEPLOY_URL="https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-$ARCH.AppImage"
LINUXDEPLOY_SHA256="c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d"
PLUGIN_URL="https://github.com/linuxdeploy/linuxdeploy-plugin-appimage/releases/download/1-alpha-20250213-1/linuxdeploy-plugin-appimage-$ARCH.AppImage"
PLUGIN_SHA256="992d502a248e14ab185448ddf6f6e7d25558cb84d4623c354c3af350c25fccb3"

verify_and_enable() { # path expected-sha256 label
  local actual
  [ -f "$1" ] || { echo "error: missing $3 at $1" >&2; exit 1; }
  [ ! -L "$1" ] || { echo "error: refusing symlinked $3" >&2; exit 1; }
  actual="$(sha256sum "$1" | awk '{print $1}')"
  [ "$actual" = "$2" ] || {
    echo "error: $3 checksum mismatch" >&2
    exit 1
  }
  chmod 0755 "$1"
}

fetch_verified() { # url expected-sha256 dest label
  local tmp="$3.download"
  command -v curl >/dev/null 2>&1 || {
    echo "error: curl is required to fetch verified AppImage tooling" >&2
    exit 1
  }
  rm -f "$tmp"
  curl --fail --location --silent --show-error \
    --proto '=https' --proto-redir '=https' \
    -o "$tmp" "$1"
  verify_and_enable "$tmp" "$2" "$4"
  mv "$tmp" "$3"
}

LD="${LINUXDEPLOY:-$WORK/linuxdeploy-$ARCH.AppImage}"
if [ -n "${LINUXDEPLOY:-}" ]; then
  verify_and_enable "$LD" "$LINUXDEPLOY_SHA256" "linuxdeploy"
else
  echo "==> downloading pinned linuxdeploy"
  fetch_verified "$LINUXDEPLOY_URL" "$LINUXDEPLOY_SHA256" "$LD" "linuxdeploy"
fi

PLUGIN="${LINUXDEPLOY_PLUGIN_APPIMAGE:-$WORK/linuxdeploy-plugin-appimage-$ARCH.AppImage}"
if [ -n "${LINUXDEPLOY_PLUGIN_APPIMAGE:-}" ]; then
  verify_and_enable "$PLUGIN" "$PLUGIN_SHA256" "linuxdeploy appimage plugin"
else
  echo "==> downloading pinned linuxdeploy appimage plugin"
  fetch_verified "$PLUGIN_URL" "$PLUGIN_SHA256" "$PLUGIN" "linuxdeploy appimage plugin"
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
