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
# dlopen'd libraries are invisible to linuxdeploy: it follows ELF NEEDED entries
# only, but winit loads the xkbcommon and X11 input/cursor libraries at runtime
# (xkbcommon-dl / x11-dl), so an AppImage built from ldd alone panics at startup
# on an X11 host that lacks libxkbcommon-x11 ("Library libxkbcommon-x11.so could
# not be loaded"). BUNDLED_DLOPEN_LIBS below is deployed explicitly; a missing
# one fails the build instead of shipping a bundle that dies on a minimal host.
# dist/appimage/smoke-test.sh audits the shipped binary's dlopen set against the
# same classification (bundled vs host-provided) and fails on any unclassified
# name, so a dependency bump that adds a dlopen'd library cannot slip through.
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
# `continuous` channel: these checksums are the verification boundary before either
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

# Libraries the binary loads with dlopen at runtime that are bundled because
# the host may lack them (a minimal X11 host has libxkbcommon but often not
# libxkbcommon-x11, libXcursor, or libXi). Keep in sync with the classification
# in dist/appimage/smoke-test.sh (the smoke test fails if they diverge from what
# the binary actually loads).
BUNDLED_DLOPEN_LIBS=(
  libxkbcommon-x11.so.0
  libXcursor.so.1
  libXi.so.6
)

# Resolve a soname to the build host's file path (linuxdeploy --library takes a
# path). A missing library is a build error, never a silent omission.
resolve_soname() {
  local soname="$1" path
  path="$( { /sbin/ldconfig -p 2>/dev/null || ldconfig -p 2>/dev/null; } |
    awk -v n="$soname" '$1 == n && /x86-64/ { print $NF; exit }')"
  if [ -z "$path" ] || [ ! -f "$path" ]; then
    echo "error: cannot find $soname on the build host (install its runtime package)" >&2
    exit 1
  fi
  # Return the soname-named path unresolved: linuxdeploy deploys a symlinked
  # path under that name, while the resolved real file would land as
  # libfoo.so.N.M.P and dlopen("libfoo.so.N") would not find it.
  printf '%s\n' "$path"
}

# libxkbcommon.so.0 stays with the host: every X11 and Wayland desktop has it,
# and a copy from the build host's older release would override a newer host
# copy and could fail to compile a newer compositor keymap. libxkbcommon-x11
# needs it, so linuxdeploy would otherwise deploy it as a dependency; exclude it
# explicitly and let the bundled libxkbcommon-x11 resolve it from the host.
LIBRARY_ARGS=(--exclude-library libxkbcommon.so.0)
for soname in "${BUNDLED_DLOPEN_LIBS[@]}"; do
  LIBRARY_ARGS+=(--library "$(resolve_soname "$soname")")
done

echo "==> bundling with linuxdeploy"
"$LD" --appimage-extract-and-run \
  --appdir "$APPDIR" \
  --executable "$BIN" \
  "${LIBRARY_ARGS[@]}" \
  --desktop-file dist/linux/io.unfinished_works.odytty.desktop \
  --icon-file dist/icons/hicolor/256x256/apps/io.unfinished_works.odytty.png \
  --output appimage

# Each explicitly deployed library must have landed in the AppDir under its
# soname (linuxdeploy renames nothing, but verify rather than assume), and the
# host-provided libxkbcommon.so.0 must not have come along as a dependency.
for soname in "${BUNDLED_DLOPEN_LIBS[@]}"; do
  if [ ! -e "$APPDIR/usr/lib/$soname" ]; then
    echo "error: $soname was not bundled into the AppDir" >&2
    exit 1
  fi
done
if [ -e "$APPDIR/usr/lib/libxkbcommon.so.0" ]; then
  echo "error: libxkbcommon.so.0 was bundled; it must stay host-provided" >&2
  exit 1
fi

# linuxdeploy writes OUTPUT into the cwd (repo root).
if [ ! -f "$OUTPUT" ]; then
  echo "error: expected $OUTPUT was not produced" >&2
  exit 1
fi
chmod +x "$OUTPUT"
echo "==> built $OUTPUT"
ls -lh "$OUTPUT"
