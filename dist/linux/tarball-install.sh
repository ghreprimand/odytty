#!/usr/bin/env sh
# Slim local installer for the OdyTTY prebuilt Linux tarball.
#
# Run this from the extracted tarball directory. It copies the odytty binary
# and its desktop-integration files (the .desktop launcher, AppStream metainfo,
# and the hicolor icon set) into a prefix, then refreshes the desktop database
# and icon cache if those tools are present. Nothing is downloaded; this only
# installs the files shipped in the tarball.
#
# Usage:
#   ./install.sh                          install into ~/.local (no root needed)
#   PREFIX=/usr/local sudo ./install.sh   system-wide install
#   ./install.sh --uninstall              remove a previous install from PREFIX
#
# PREFIX defaults to ~/.local so a normal user can install without sudo; the
# binary lands in PREFIX/bin (ensure that is on your PATH). Set PREFIX and use
# sudo for a system-wide install under /usr/local.
set -eu

SELF_DIR="$(cd "$(dirname "$0")" && pwd)"
PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"
DATA_DIR="$PREFIX/share"

DESKTOP_ID="io.unfinished_works.odytty"

uninstall() {
    rm -f "$BIN_DIR/odytty"
    rm -f "$DATA_DIR/applications/$DESKTOP_ID.desktop"
    rm -f "$DATA_DIR/metainfo/$DESKTOP_ID.metainfo.xml"
    find "$DATA_DIR/icons/hicolor" -name "$DESKTOP_ID.*" -delete 2>/dev/null || true
    echo "Removed OdyTTY from $PREFIX."
}

if [ "${1:-}" = "--uninstall" ]; then
    uninstall
    exit 0
fi

if [ ! -x "$SELF_DIR/odytty" ]; then
    echo "error: odytty binary not found next to this script; run it from the extracted tarball." >&2
    exit 1
fi

echo "Installing OdyTTY into $PREFIX"
install -Dm755 "$SELF_DIR/odytty" "$BIN_DIR/odytty"

if [ -f "$SELF_DIR/share/applications/$DESKTOP_ID.desktop" ]; then
    install -Dm644 "$SELF_DIR/share/applications/$DESKTOP_ID.desktop" \
        "$DATA_DIR/applications/$DESKTOP_ID.desktop"
fi
if [ -f "$SELF_DIR/share/metainfo/$DESKTOP_ID.metainfo.xml" ]; then
    install -Dm644 "$SELF_DIR/share/metainfo/$DESKTOP_ID.metainfo.xml" \
        "$DATA_DIR/metainfo/$DESKTOP_ID.metainfo.xml"
fi
if [ -d "$SELF_DIR/share/icons/hicolor" ]; then
    ( cd "$SELF_DIR/share/icons/hicolor" && find . -type f -print ) | while IFS= read -r icon; do
        install -Dm644 "$SELF_DIR/share/icons/hicolor/$icon" "$DATA_DIR/icons/hicolor/$icon"
    done
fi

# Refresh caches when the tools exist (harmless to skip; the launcher still
# works, it just may not appear in a menu until the next login).
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$DATA_DIR/applications" >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -t "$DATA_DIR/icons/hicolor" >/dev/null 2>&1 || true
fi

echo "Installed odytty to $BIN_DIR/odytty"
case ":$PATH:" in
    *":$BIN_DIR:"*) : ;;
    *) echo "note: $BIN_DIR is not on your PATH; add it to run 'odytty' by name." ;;
esac
