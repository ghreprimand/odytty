#!/bin/sh
# Runs inside a clean ubuntu:22.04 container (release workflow):
#
#   docker run --rm -v "$PWD:/w:ro" ubuntu:22.04 \
#     sh /w/dist/appimage/smoke-test-container.sh /w/odytty-<version>-x86_64.AppImage
#
# Installs only Xvfb, a software Vulkan driver (lavapipe), and the libraries the
# AppImage deliberately leaves to the host (see HOST_PROVIDED in smoke-test.sh).
# It installs libxkbcommon0 (the host-provided core library every desktop has)
# but no libxkbcommon-x11, Xcursor, or XInput package, so the AppImage can only
# start by using the libraries it bundles. smoke-test.sh refuses to run if the
# host provides any of them.
set -eu
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
  xvfb xauth libx11-6 libxkbcommon0 libfontconfig1 libfreetype6 libvulkan1 \
  mesa-vulkan-drivers ca-certificates >/dev/null
exec bash /w/dist/appimage/smoke-test.sh run "$1"
