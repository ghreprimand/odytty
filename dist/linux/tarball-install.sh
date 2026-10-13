#!/usr/bin/env sh
# Slim local installer for the OdyTTY prebuilt Linux tarball.
#
# Run this from the extracted tarball directory. It copies the odytty binary,
# its desktop-integration files (the .desktop launcher, AppStream metainfo,
# and the hicolor icon set), and the licence texts (LICENSE, NOTICE,
# THIRD-PARTY-LICENSES and the bundled fonts' licences, under
# PREFIX/share/licenses/odytty) into a prefix, then refreshes the desktop
# database and icon cache if those tools are present. Nothing is downloaded;
# this only installs the files shipped in the tarball. --uninstall removes
# only those files, never the whole licence directory.
#
# Usage:
#   ./install.sh                              install into ~/.local (no root needed)
#   sudo env PREFIX=/usr/local ./install.sh   system-wide install
#   ./install.sh --uninstall                  remove a previous install from PREFIX
#
# PREFIX defaults to ~/.local so a normal user can install without sudo; the
# binary lands in PREFIX/bin (ensure that is on your PATH). For a system-wide
# install under /usr/local, pass PREFIX through env: sudo resets the
# environment, so a PREFIX assigned before sudo never reaches the script.
# /usr is reserved for package-managed installs; neither operation accepts it,
# including paths and symlink aliases that resolve there.
set -eu

SELF_DIR="$(cd "$(dirname "$0")" && pwd)"
PREFIX="${PREFIX:-$HOME/.local}"
# Resolve before any mutation, including uninstall. GNU realpath -m also
# handles a fresh prefix whose directories have not been created yet.
if ! RESOLVED_PREFIX="$(realpath -m -- "$PREFIX")"; then
    echo "error: cannot resolve PREFIX; no files changed." >&2
    exit 1
fi
if [ "$RESOLVED_PREFIX" = "/usr" ]; then
    echo "error: /usr is reserved for package-managed installations; use the package manager, or choose ~/.local or /usr/local for the tarball." >&2
    exit 1
fi
BIN_DIR="$PREFIX/bin"
DATA_DIR="$PREFIX/share"

DESKTOP_ID="io.unfinished_works.odytty"
LICENSE_DIR="$DATA_DIR/licenses/odytty"

# Remove only the licence files this tarball installs: the named notices and the
# font licence files listed in the tarball's own licenses/fonts tree. Never a
# recursive delete: custom prefixes can contain other licence files, and a
# file this installer did not put there must survive. A
# directory goes only once it is empty. Run from the extracted tarball, as the
# install is.
uninstall_licences() {
    for notice in LICENSE NOTICE THIRD-PARTY-LICENSES; do
        rm -f "$LICENSE_DIR/$notice"
    done
    if [ -d "$SELF_DIR/licenses/fonts" ]; then
        ( cd "$SELF_DIR/licenses/fonts" && find . -type f -print ) | while IFS= read -r file; do
            rm -f "$LICENSE_DIR/fonts/$file"
        done
    fi
    if [ -d "$LICENSE_DIR/fonts" ]; then
        find "$LICENSE_DIR/fonts" -depth -type d -exec rmdir {} + 2>/dev/null || true
    fi
    rmdir "$LICENSE_DIR" 2>/dev/null || true
}

uninstall() {
    rm -f "$BIN_DIR/odytty"
    rm -f "$DATA_DIR/applications/$DESKTOP_ID.desktop"
    rm -f "$DATA_DIR/metainfo/$DESKTOP_ID.metainfo.xml"
    find "$DATA_DIR/icons/hicolor" -name "$DESKTOP_ID.*" -delete 2>/dev/null || true
    uninstall_licences
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
# The licence texts travel with the binary: the GPL, the notices, and the
# bundled fonts' licences, which require their notices with every copy. A
# notice file a tarball does not carry is skipped.
for notice in LICENSE NOTICE THIRD-PARTY-LICENSES; do
    if [ -f "$SELF_DIR/$notice" ]; then
        install -Dm644 "$SELF_DIR/$notice" "$LICENSE_DIR/$notice"
    fi
done
if [ -d "$SELF_DIR/licenses/fonts" ]; then
    ( cd "$SELF_DIR/licenses/fonts" && find . -type f -print ) | while IFS= read -r file; do
        install -Dm644 "$SELF_DIR/licenses/fonts/$file" "$LICENSE_DIR/fonts/$file"
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
