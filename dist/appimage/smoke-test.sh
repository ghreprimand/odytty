#!/usr/bin/env bash
# Smoke tests for a built OdyTTY AppImage.
#
# Usage:
#   dist/appimage/smoke-test.sh audit <AppImage>
#   dist/appimage/smoke-test.sh run   <AppImage>
#
# `--version` exits before any windowing code, so it cannot see a library that
# the binary loads with dlopen at startup (winit loads xkbcommon-x11, Xlib,
# Xcursor, and XInput at runtime; linuxdeploy follows ELF NEEDED entries only).
# These two modes close that gap:
#
#   audit  Extracts the AppImage and lists every shared-library name the shipped
#          binary can load. Each name must be classified below as bundled (and
#          then must exist in the AppImage) or provided by the host. An
#          unclassified name fails: a dependency bump that adds a dlopen'd
#          library has to be decided on purpose. Needs no display and no GPU.
#
#   run    Starts the AppImage under Xvfb with an X11-only environment and
#          requires the process to still be alive after a few seconds. Run it
#          on a host that lacks every bundled library (the release workflow uses
#          a clean ubuntu:22.04 container with only xvfb, a software Vulkan
#          driver, and the host-provided libraries below). The run mode refuses
#          to start if the host provides a bundled library, so a pass cannot
#          come from the host's copy.
#
# Both modes extract with the AppImage's own `--appimage-extract`, so no FUSE
# is needed.
set -euo pipefail

# Loaded with dlopen and shipped inside the AppImage. Must match
# BUNDLED_DLOPEN_LIBS in build-appimage.sh.
BUNDLED=(
  libxkbcommon-x11.so.0
  libXcursor.so.1
  libXi.so.6
)

# Provided by the host on purpose: glibc and the compiler runtime; the core
# xkbcommon library (every desktop has it, and an older bundled copy could
# override a newer host copy and fail on a newer compositor keymap); the display
# server client libraries (X11/xcb and Wayland) of whichever session is in use;
# the graphics loaders (the driver stack must match the user's GPU); fontconfig
# and freetype (on the linuxdeploy exclude list); and libdbus, which only the
# native file-dialog portal path loads and which fails soft when absent.
HOST_PROVIDED=(
  libxkbcommon.so.0
  libc.so.6
  libm.so.6
  libgcc_s.so.1
  libX11.so.6
  libX11-xcb.so.1
  libxcb.so.1
  libwayland-client.so.0
  libwayland-egl.so.1
  libvulkan.so.1
  libEGL.so.1
  libfontconfig.so.1
  libfreetype.so.6
  libdbus-1.so.3
)

die() { echo "FAIL: $*" >&2; exit 1; }

in_list() { # name list...
  local needle="$1" item
  shift
  for item in "$@"; do [ "$item" = "$needle" ] && return 0; done
  return 1
}

mode="${1:-}"
image="${2:-}"
if [ -z "$mode" ] || [ -z "$image" ]; then
  die "usage: $0 audit|run <AppImage>"
fi
[ -f "$image" ] || die "missing AppImage: $image"

WORK="$(mktemp -d)"
trap 'kill "${XVFB_PID:-}" 2>/dev/null || true; rm -rf "$WORK"' EXIT
cp "$image" "$WORK/odytty.AppImage"
chmod +x "$WORK/odytty.AppImage"
(cd "$WORK" && ./odytty.AppImage --appimage-extract >/dev/null)
ROOT="$WORK/squashfs-root"
BIN="$ROOT/usr/bin/odytty"
[ -x "$BIN" ] || die "no usr/bin/odytty in the extracted AppImage"

audit() {
  local names name missing=0 unknown=0
  # Versioned sonames only: the unversioned names are fallbacks of the same
  # libraries and never the only way to load one. The length bound keeps the
  # match from swallowing the long run of Xlib symbol names that sits next to
  # one of the names in the read-only data.
  names="$(grep -a -o -E 'lib[A-Za-z0-9_+-]{1,40}\.so(\.[0-9]+)+' "$BIN" | sort -u)"
  [ -n "$names" ] || die "found no library names in the binary (audit is broken)"
  echo "library names in the binary:"
  while IFS= read -r name; do
    if in_list "$name" "${BUNDLED[@]}"; then
      if [ -e "$ROOT/usr/lib/$name" ]; then
        echo "  bundled   $name"
      else
        echo "  MISSING   $name (classified as bundled but absent from the AppImage)"
        missing=1
      fi
    elif in_list "$name" "${HOST_PROVIDED[@]}"; then
      echo "  host      $name"
    else
      echo "  UNKNOWN   $name"
      unknown=1
    fi
  done <<<"$names"
  [ ! -e "$ROOT/usr/lib/libxkbcommon.so.0" ] || die "libxkbcommon.so.0 is bundled; it must stay host-provided"
  # Every bundled library must also be something the binary actually loads, so
  # a stale entry cannot hide behind a passing audit.
  for name in "${BUNDLED[@]}"; do
    grep -a -q -F "$name" "$BIN" || die "$name is classified as bundled but the binary never loads it; update the lists"
  done
  [ "$unknown" = 0 ] || die "unclassified library names above: bundle them or add them to HOST_PROVIDED with a reason (here and in build-appimage.sh)"
  [ "$missing" = 0 ] || die "bundled libraries are missing from the AppImage"
  echo "PASS: dlopen audit"
}

run() {
  local name found=0 status
  for name in "${BUNDLED[@]}"; do
    if { /sbin/ldconfig -p 2>/dev/null || ldconfig -p 2>/dev/null; } | awk -v n="$name" '$1 == n { f = 1 } END { exit !f }'; then
      echo "host provides $name" >&2
      found=1
    fi
  done
  [ "$found" = 0 ] || die "the host provides bundled libraries, so this run would not prove the bundle; use a clean host or container"
  command -v Xvfb >/dev/null 2>&1 || die "Xvfb is required for the run mode"

  export DISPLAY=:97
  export HOME="$WORK/home" XDG_RUNTIME_DIR="$WORK/xdg"
  mkdir -p "$HOME" "$XDG_RUNTIME_DIR"
  chmod 700 "$XDG_RUNTIME_DIR"
  unset WAYLAND_DISPLAY
  Xvfb "$DISPLAY" -screen 0 1280x800x24 >"$WORK/xvfb.log" 2>&1 &
  XVFB_PID=$!
  sleep 2
  kill -0 "$XVFB_PID" 2>/dev/null || die "Xvfb did not start: $(cat "$WORK/xvfb.log")"

  # A healthy start keeps running until the timeout kills it (exit 124). Any
  # earlier exit is a startup failure; the output is printed either way.
  set +e
  timeout 12 "$ROOT/AppRun" >"$WORK/run.log" 2>&1
  status=$?
  set -e
  grep -v '^ *[0-9]*: ' "$WORK/run.log" | head -n 30 || true
  [ "$status" = 124 ] || die "the AppImage exited early with status $status instead of staying up"
  if grep -q -E 'could not be loaded|PANIC' "$WORK/run.log"; then
    die "the run log reports a missing library or a panic"
  fi
  echo "PASS: AppImage stayed up under X11 with the bundled libraries only"
}

case "$mode" in
  audit) audit ;;
  run) run ;;
  *) die "unknown mode: $mode" ;;
esac
