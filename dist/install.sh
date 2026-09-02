#!/usr/bin/env bash
# OdyTTY one-line installer for Linux x86_64.
#
# Download the version-pinned installer release asset, review it, then run it:
#   curl -fLO https://github.com/ghreprimand/odytty/releases/download/vX.Y.Z/odytty-X.Y.Z-install.sh
#   bash odytty-X.Y.Z-install.sh
#
# Detects the package manager and installs the matching prebuilt artifact from
# the latest GitHub release: a native .deb (apt/dpkg), a native .rpm (dnf/rpm),
# or the portable binary tarball otherwise. The downloaded artifact is always
# signature-verified SHA256SUMS before anything is installed.
#
# This is Linux x86_64 only. On macOS it prints the Homebrew command; on Windows
# it prints the Scoop command. Other architectures are pointed at the AppImage
# or a source build. No telemetry, no tokens, nothing is sent anywhere.
#
#   --dry-run   print the planned actions and exit without downloading or
#               installing (used by CI to smoke-test the script offline)
#   --insecure-skip-signature
#                skip Minisign manifest authentication (not recommended; trusts
#                the delivery channel for SHA256SUMS)
#   --help       show this help
set -euo pipefail

REPO="ghreprimand/odytty"
BASE_URL="https://github.com/${REPO}/releases/latest/download"
DRY_RUN=0
INSECURE_SKIP_SIGNATURE=0
MINISIGN_PUBLIC_KEY='RWQcOPw3PisdAGt2Q2IF7W6P1sgyPs2b9rQvFJohmLC8/w+qJt+aXEev'

usage() {
    sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --dry-run) DRY_RUN=1 ;;
        --insecure-skip-signature) INSECURE_SKIP_SIGNATURE=1 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1 (try --help)" >&2; exit 2 ;;
    esac
    shift
done

say()  { printf '%s\n' "$*"; }
err()  { printf 'error: %s\n' "$*" >&2; }
have() { command -v "$1" >/dev/null 2>&1; }

# --- platform gate ----------------------------------------------------------
os="$(uname -s)"
case "$os" in
    Linux) : ;;
    Darwin)
        say "OdyTTY on macOS installs via Homebrew:"
        say "  brew tap ghreprimand/odytty"
        say "  brew install --cask odytty"
        exit 0
        ;;
    MINGW*|MSYS*|CYGWIN*|Windows_NT)
        say "OdyTTY on Windows installs via Scoop:"
        say "  scoop bucket add odytty https://github.com/${REPO}"
        say "  scoop install odytty"
        exit 0
        ;;
    *)
        err "unsupported operating system: $os"
        say "OdyTTY ships prebuilt binaries for Linux, macOS, and Windows."
        exit 1
        ;;
esac

arch="$(uname -m)"
case "$arch" in
    x86_64|amd64) : ;;
    *)
        err "unsupported architecture: $arch"
        say "Prebuilt Linux packages are x86_64 only. On $arch, build from source"
        say "(https://github.com/${REPO}) or run the portable AppImage."
        exit 1
        ;;
esac

# --- choose artifact + install command --------------------------------------
# Aliases are the always-latest names published alongside every release; using
# releases/latest/download keeps this script version-agnostic.
if have apt-get || have dpkg; then
    kind="deb"
    artifact="odytty-amd64.deb"
elif have dnf || have rpm; then
    kind="rpm"
    artifact="odytty-x86_64.rpm"
else
    kind="tarball"
    artifact="odytty-linux-x86_64.tar.gz"
fi

url="${BASE_URL}/${artifact}"
sums_url="${BASE_URL}/SHA256SUMS"
signature_url="${BASE_URL}/SHA256SUMS.minisig"

# sudo policy: root installs directly; a non-root user with sudo uses it for the
# system package managers and for a /usr/local tarball install; without sudo the
# tarball falls back to a per-user ~/.local install (the .deb/.rpm managers need
# root, so we say so and stop).
SUDO=()
if [ "$(id -u)" -ne 0 ] && have sudo; then
    SUDO=(sudo)
fi

sudo_note=""
if [ "${#SUDO[@]}" -gt 0 ]; then sudo_note=" via sudo"; fi

describe_install() {
    case "$kind" in
        deb)     say "  install with apt/dpkg (needs root$sudo_note)" ;;
        rpm)     say "  install with dnf/rpm (needs root$sudo_note)" ;;
        tarball)
            if [ "${#SUDO[@]}" -gt 0 ] || [ "$(id -u)" -eq 0 ]; then
                say "  install the binary + desktop files under /usr/local$sudo_note"
            else
                say "  install the binary + desktop files under ~/.local (no sudo available)"
            fi
            ;;
    esac
}

if [ "$DRY_RUN" -eq 1 ]; then
    say "OdyTTY installer plan (dry run):"
    say "  os=$os arch=$arch manager=$kind"
    say "  download $url"
    say "  verify   SHA256SUMS.minisig against the pinned OdyTTY release key"
    say "  verify   $artifact against the authenticated SHA256SUMS"
    describe_install
    exit 0
fi

# --- download ---------------------------------------------------------------
dl() { # url dest
    if have curl; then
        curl -fSL --retry 3 -o "$2" "$1"
    elif have wget; then
        wget -O "$2" "$1"
    else
        err "need curl or wget to download"; exit 1
    fi
}

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"

say "Downloading $artifact ..."
dl "$url" "$artifact"
say "Downloading SHA256SUMS ..."
dl "$sums_url" "SHA256SUMS"
if [ "$INSECURE_SKIP_SIGNATURE" -eq 0 ]; then
    if ! have minisign; then
        err "minisign is required to authenticate SHA256SUMS; install it and retry."
        err "Use --insecure-skip-signature only if you accept trusting the download channel."
        exit 1
    fi
    say "Downloading SHA256SUMS.minisig ..."
    dl "$signature_url" "SHA256SUMS.minisig"
    say "Authenticating SHA256SUMS with the pinned OdyTTY release key ..."
    minisign -Vm SHA256SUMS -x SHA256SUMS.minisig -P "$MINISIGN_PUBLIC_KEY"
else
    err "signature verification skipped; trusting the download channel for SHA256SUMS."
fi

# --- verify -----------------------------------------------------------------
want="$(awk -v f="$artifact" '$2 == f { print $1 }' SHA256SUMS | head -n1)"
if [ -z "$want" ]; then
    err "no SHA256 entry for $artifact in SHA256SUMS; aborting."
    exit 1
fi
if have sha256sum; then
    got="$(sha256sum "$artifact" | awk '{print $1}')"
elif have shasum; then
    got="$(shasum -a 256 "$artifact" | awk '{print $1}')"
else
    err "need sha256sum or shasum to verify the download"; exit 1
fi
if [ "$want" != "$got" ]; then
    err "checksum mismatch for $artifact"
    err "  expected $want"
    err "  got      $got"
    exit 1
fi
say "Checksum verified."

# --- install ----------------------------------------------------------------
case "$kind" in
    deb)
        if have apt-get; then
            "${SUDO[@]}" apt-get install -y "./$artifact"
        else
            "${SUDO[@]}" dpkg -i "$artifact" || "${SUDO[@]}" apt-get -f install -y
        fi
        ;;
    rpm)
        if have dnf; then
            "${SUDO[@]}" dnf install -y "./$artifact"
        else
            "${SUDO[@]}" rpm -i --replacepkgs "$artifact"
        fi
        ;;
    tarball)
        dir="$(tar -tzf "$artifact" | head -n1 | cut -d/ -f1)"
        tar -xzf "$artifact"
        if [ ! -x "$dir/install.sh" ]; then
            err "tarball missing install.sh; aborting."
            exit 1
        fi
        if [ "$(id -u)" -eq 0 ]; then
            PREFIX=/usr/local "$dir/install.sh"
        elif [ "${#SUDO[@]}" -gt 0 ]; then
            "${SUDO[@]}" env PREFIX=/usr/local "$dir/install.sh"
        else
            PREFIX="$HOME/.local" "$dir/install.sh"
        fi
        ;;
esac

say "OdyTTY installed. Run: odytty"
