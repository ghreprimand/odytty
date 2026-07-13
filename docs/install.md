# Installing OdyTTY

OdyTTY ships as a versioned release. Each release provides:

- a git tag (`vX.Y.Z`) and source tarball;
- a GitHub Release entry with `SHA256SUMS` for every artifact;
- a best-effort x86_64 **AppImage** (one download for most Linux distributions);
- an **AUR** package (`odytty`) for Arch-family systems;
- a prebuilt **macOS** `.app` zip for Apple Silicon (ad-hoc signed) plus a Homebrew tap;
- an unsigned Windows x86_64 portable **zip**;
- source-build instructions for Odyssey/LFS and other developer systems;
- a desktop entry, AppStream metadata, and icon installed into Freedesktop
  locations.

Pick by system: on Linux the one-line installer picks a native `.deb` or `.rpm`
for you (with the AppImage as a no-install single-file fallback and the AUR
package on Arch-family systems), Homebrew on macOS, Scoop (or a direct zip
download) on Windows, and a pacman-tracked source package on Odyssey itself so
the install is versioned, owned, removable, and visible to Odyssey-Mon.

## Release Artifact Names And Checksums

Packaged downloads are published under a stable always-latest alias and a
version-pinned copy:

| Always-latest alias | Version-pinned copy |
| --- | --- |
| `odytty-amd64.deb` | `odytty-<version>-amd64.deb` |
| `odytty-x86_64.rpm` | `odytty-<version>-x86_64.rpm` |
| `odytty-linux-x86_64.tar.gz` | `odytty-<version>-linux-x86_64.tar.gz` |
| `odytty-x86_64.AppImage` | `odytty-<version>-x86_64.AppImage` |
| `odytty-macos-arm64.zip` | `odytty-<version>-macos-arm64.zip` |
| `odytty-windows-x86_64.zip` | `odytty-<version>-windows-x86_64.zip` |
| `odytty.tar.gz` | `odytty-<version>.tar.gz` |

Each alias and its version-pinned twin are byte-identical and therefore have
matching hashes in `SHA256SUMS`. Durable links should use the aliases under
`releases/latest/download/`; pinned names are for selecting one specific
release.

## Linux

Linux is the primary and most battle-tested platform, and it needs a
Vulkan-capable GPU. Wayland is the primary display target. X11 works through
the current `winit` and GPU stack, with some window-manager-dependent behavior
for borderless windows and OS theme detection.

### One-line installer (recommended)

The fastest path. It detects your package manager and installs the matching
prebuilt artifact from the latest release:

```sh
curl -fsSL https://raw.githubusercontent.com/ghreprimand/odytty/master/dist/install.sh | bash
```

It chooses a native `.deb` on apt/dpkg systems, a native `.rpm` on dnf/rpm
systems, or the portable binary tarball otherwise. The download is always
checksum-verified against the release `SHA256SUMS` before anything is
installed. System package managers need root, so the script uses `sudo` when
you are not already root; the tarball path falls back to a per-user `~/.local`
install when no `sudo` is available. Pass `--dry-run` to print the plan and exit
without downloading or installing:

```sh
curl -fsSL https://raw.githubusercontent.com/ghreprimand/odytty/master/dist/install.sh | bash -s -- --dry-run
```

It is Linux x86_64 only: on macOS it prints the Homebrew command and on Windows
the Scoop command instead of installing, and other architectures are pointed at
the AppImage or a source build.

### .deb (Debian, Ubuntu, Mint, Pop)

Download the always-latest `.deb` alias and its checksums, verify, and install
with apt so dependencies resolve:

```sh
curl -LO https://github.com/ghreprimand/odytty/releases/latest/download/odytty-amd64.deb
curl -LO https://github.com/ghreprimand/odytty/releases/latest/download/SHA256SUMS
sha256sum -c SHA256SUMS --ignore-missing
sudo apt install ./odytty-amd64.deb
```

The package installs the binary, desktop entry, AppStream metadata, and icons
under dpkg ownership. Update with a normal `sudo apt upgrade` once a newer
release publishes, or re-run the download-and-install above.

### .rpm (Fedora, RHEL, openSUSE, best-effort)

Download the always-latest `.rpm` alias, verify, and install with dnf:

```sh
curl -LO https://github.com/ghreprimand/odytty/releases/latest/download/odytty-x86_64.rpm
curl -LO https://github.com/ghreprimand/odytty/releases/latest/download/SHA256SUMS
sha256sum -c SHA256SUMS --ignore-missing
sudo dnf install ./odytty-x86_64.rpm
```

Like the AppImage, the `.rpm` is a best-effort artifact: it is cross-built on
Ubuntu and its metadata is validated in CI, but it is not tested on every RPM
distribution. If it does not install cleanly on your system, use the binary
tarball or build from source.

### Binary tarball (portable prebuilt)

The `odytty-linux-x86_64.tar.gz` is a prebuilt binary plus desktop-integration
files and a bundled `install.sh`, for systems where a native package does not
fit. Download, verify, extract, and run the bundled installer:

```sh
curl -LO https://github.com/ghreprimand/odytty/releases/latest/download/odytty-linux-x86_64.tar.gz
curl -LO https://github.com/ghreprimand/odytty/releases/latest/download/SHA256SUMS
sha256sum -c SHA256SUMS --ignore-missing
tar -xzf odytty-linux-x86_64.tar.gz
cd odytty-*-linux-x86_64
./install.sh
```

The bundled `install.sh` copies the binary and desktop files into a prefix and
refreshes the desktop and icon caches when those tools are present. `PREFIX`
defaults to `~/.local` for a no-root per-user install (make sure
`~/.local/bin` is on your `PATH`); set `PREFIX=/usr/local` and use `sudo` for a
system-wide install. Remove a previous install with `./install.sh --uninstall`
(honoring the same `PREFIX`).

### AppImage (x86_64)

Download the always-latest AppImage alias and checksum file, verify it, mark it
executable, and run it:

```sh
curl -LO https://github.com/ghreprimand/odytty/releases/latest/download/odytty-x86_64.AppImage
curl -LO https://github.com/ghreprimand/odytty/releases/latest/download/SHA256SUMS
sha256sum -c SHA256SUMS --ignore-missing
chmod +x odytty-x86_64.AppImage
./odytty-x86_64.AppImage
```

The executable bit is required because browsers normally omit it. Without
`chmod +x`, the file may open in an archive viewer or fail with
"permission denied".

The AppImage bundles OdyTTY's own dependencies but **not** the graphics driver:
it uses the host's Vulkan ICD (Mesa or a vendor driver), so the machine needs a
working Vulkan setup - the same requirement as a source build. It is built on
the oldest supported Ubuntu LTS for a wide glibc floor. This is a best-effort
artifact; if Vulkan initialization fails on your host, build from source.

To integrate it into menus, tools like [Gear Lever][gearlever] or
`appimaged` register the bundled desktop entry and icon. The AppImage can be
built locally with `dist/appimage/build-appimage.sh`.

[gearlever]: https://github.com/mijorus/gearlever

### Arch Linux (AUR)

Install the `odytty` package with an AUR helper:

```sh
paru -S odytty      # or: yay -S odytty
```

Or install manually:

```sh
git clone https://aur.archlinux.org/odytty.git
cd odytty
makepkg -si
```

The package builds from the release source tarball and installs the binary,
desktop entry, AppStream metadata, and icons under pacman ownership. The
first install compiles from source (pulling in `cargo`/`rust`), so it takes a
few minutes. The AUR package is community-maintained and tracks the published
GitHub releases; it usually updates shortly after a release, though timing
depends on its maintainer. It is not an official project release channel, and
AUR packages are not vetted by Arch. Review the PKGBUILD before installing; AUR
helpers show it by default.

To pick up the very latest immediately regardless of the AUR package's state,
build from the release source tarball directly (the PKGBUILD template and its
publish runbook live in `dist/aur/`), or use the always-latest download links
below.

### Updating

Native package installs update with your system package manager once a newer
release publishes: `sudo apt upgrade` (deb) or `sudo dnf upgrade` (rpm).
Re-running the one-line installer also pulls and installs the newest package:

```sh
curl -fsSL https://raw.githubusercontent.com/ghreprimand/odytty/master/dist/install.sh | bash
```

The AUR package updates with a normal `paru -Syu` / `yay -Syu`.

The AppImage and binary tarball have no package manager, so an update is a
re-download of the always-latest alias (it resolves to the newest release). For
the AppImage:

```sh
curl -LO https://github.com/ghreprimand/odytty/releases/latest/download/odytty-x86_64.AppImage
curl -LO https://github.com/ghreprimand/odytty/releases/latest/download/SHA256SUMS
sha256sum -c SHA256SUMS --ignore-missing
chmod +x odytty-x86_64.AppImage
```

For the binary tarball, re-download `odytty-linux-x86_64.tar.gz`, verify, and
re-run its bundled `./install.sh` with the same `PREFIX` as before.

## macOS (Apple Silicon)

macOS builds and passes the full test suite on every push, and the release
ships a prebuilt `OdyTTY.app` for Apple Silicon (arm64). The binary is
**ad-hoc code-signed** in CI (`codesign -s -`), which needs no Apple Developer
account and no notarization - enough for the app to launch, but not an Apple
Developer identity. There is no `.dmg` and no Gatekeeper-approved signature yet.

### Homebrew (recommended)

The Homebrew tap is the least-friction path; the cask handles Gatekeeper
approval for you (see below). Add the tap and install the cask:

```sh
brew tap ghreprimand/odytty
brew install --cask odytty
```

Recent Homebrew versions require trusting a third-party tap before its cask will load. If the install stops with `Refusing to load cask ... from untrusted tap`, run `brew trust ghreprimand/odytty` (or `brew trust --cask ghreprimand/odytty/odytty` to trust just this cask) and re-run the install. It is a one-time per-machine trust.

The **cask** installs the prebuilt, ad-hoc-signed `OdyTTY.app` into
`/Applications`. The app is ad-hoc signed but not notarized, so macOS
quarantines the download and Gatekeeper would otherwise block the first
launch. To spare you a manual step, the cask automatically clears the
`com.apple.quarantine` security attribute from the installed app during
install (the same `xattr -dr com.apple.quarantine` you would otherwise run by
hand), so `brew install --cask` and `brew upgrade` launch cleanly. Be aware
that this removes a macOS Gatekeeper safeguard on an ad-hoc-signed,
un-notarized app; the cask's install-time caveats disclose it in the terminal.
Notarization through the Apple Developer Program (deferred) is the only way to
avoid needing that flag-clear at all. Because the app lands
in `/Applications`, it appears in Launchpad and Spotlight and can be dragged to
the Dock to pin it, with no separate launcher step. (The source formula below
installs only the `odytty` CLI on your PATH, with no GUI launcher.)

Prefer to compile locally instead? Use the **source formula**, which builds
from the release tarball with your own toolchain:

```sh
brew install ghreprimand/odytty/odytty
```

Per release there is nothing to do on your side: CI bumps the tap's cask to
each new version automatically shortly after the release publishes, so
`brew upgrade` picks it up.

### Updating

**Cask (recommended):** refresh to the newest release. `brew update` refreshes the tap first so a just-published version is visible:

```sh
brew update
brew upgrade --cask odytty
```

- **Source formula:** `brew upgrade odytty`

The scoped commands update only OdyTTY; a plain `brew upgrade` refreshes
everything Homebrew manages.

### Direct .app zip download

If you download the `.app` zip straight from the GitHub Release instead of
using Homebrew, macOS tags it with a quarantine attribute (because it arrived
from the internet and the signature is ad-hoc, not notarized). Verify the
checksum, unzip, move the app into place, then clear the quarantine flag once:

```sh
curl -L -o odytty-macos-arm64.zip https://github.com/ghreprimand/odytty/releases/latest/download/odytty-macos-arm64.zip
curl -L -o SHA256SUMS https://github.com/ghreprimand/odytty/releases/latest/download/SHA256SUMS
shasum -a 256 -c SHA256SUMS --ignore-missing
unzip odytty-macos-arm64.zip
mv OdyTTY.app /Applications/
xattr -dr com.apple.quarantine /Applications/OdyTTY.app
```

The `xattr -dr com.apple.quarantine` step is the one-time stopgap that lets an
ad-hoc-signed app past Gatekeeper without notarization. The Homebrew cask runs
the same quarantine-clearing step in its postflight, so this manual step is
only needed for a direct (non-brew) zip download.

### Build from source

A source build works with the standard Rust toolchain
([rustup](https://rustup.rs)) and the Xcode Command Line Tools. Both Apple
Silicon and Intel are supported through the target Cargo selects natively:

```sh
xcode-select --install   # once, if the Command Line Tools are not installed
cargo build --release --locked
./target/release/odytty
```

A locally compiled binary is not quarantined. To assemble a double-clickable
`OdyTTY.app`, run:

```sh
version=$(./target/release/odytty --version | awk '{print $2}')
mkdir -p dist/build
cp target/release/odytty dist/build/odytty
bash dist/macos/make-app.sh "$version"
cp -R dist/build/OdyTTY.app /Applications/
```

To run the source build as `odytty` from any shell:

```sh
mkdir -p "$HOME/.local/bin"
ln -sfn "$PWD/target/release/odytty" "$HOME/.local/bin/odytty"
```

Make sure `$HOME/.local/bin` is on `PATH`. The packaged release workflow also
ad-hoc signs its app bundle; a local source build does not need that packaging
step to run.

### Signed / notarized builds

A signed, notarized `.dmg` (no `xattr` stopgap, no first-run friction on any
download path) requires enrollment in the Apple Developer Program. That is
deferred to if/when the project opts into that program; until then the ad-hoc
signature plus Homebrew's quarantine strip is the account-free path.

## Windows

Windows support is new and still maturing - it builds and runs the full
terminal and is exercised on a Windows CI leg every push, but the polish bar is
behind Linux. Bug reports for the Windows build are especially welcome;
[open an issue](https://github.com/ghreprimand/odytty/issues) with your Windows
version and a short repro.

The Windows release is an unsigned portable `odytty.exe` inside
`odytty-windows-x86_64.zip`. There is no installer, and nothing is written
outside your profile - configuration lives under `%APPDATA%\odytty\`. Scoop is
the recommended install path because it puts `odytty` on your PATH, adds a
Start-menu entry, and verifies the download checksum; you can also download the
zip directly.

### Scoop

[Scoop](https://scoop.sh) is a per-user package manager for Windows (no admin
rights). If you don't already have it, install it once in PowerShell:

```powershell
Set-ExecutionPolicy -ExecutionPolicy RemoteSigned -Scope CurrentUser
Invoke-RestMethod -Uri https://get.scoop.sh | Invoke-Expression
```

Then add the in-repo bucket and install:

```powershell
scoop bucket add odytty https://github.com/ghreprimand/odytty
scoop install odytty
```

Scoop creates a shim under `~\scoop\shims` (on your PATH) so `odytty` launches
from any shell, and adds an **OdyTTY** Start-menu entry. The bucket manifest is
`bucket/odytty.json`: it pins the release URL and hash, which is what
`scoop update odytty` reads. From your side there is nothing to do per release -
CI bumps the manifest to each new version automatically shortly after the
release publishes, so `scoop update odytty` picks up new versions on its own once
that bump lands.

### Portable zip

Download the always-latest zip alias and checksum file, verify the hash, and
run the executable:

```powershell
Invoke-WebRequest https://github.com/ghreprimand/odytty/releases/latest/download/odytty-windows-x86_64.zip -OutFile odytty-windows-x86_64.zip
Invoke-WebRequest https://github.com/ghreprimand/odytty/releases/latest/download/SHA256SUMS -OutFile SHA256SUMS
Get-FileHash odytty-windows-x86_64.zip -Algorithm SHA256
Expand-Archive odytty-windows-x86_64.zip -DestinationPath .\odytty
.\odytty\odytty.exe
```

Compare the hash with the `odytty-windows-x86_64.zip` row in `SHA256SUMS`; they
must match before you run the binary.

Because OdyTTY is not code-signed yet, launching it for the first time may
raise a blue "Windows protected your PC" SmartScreen dialog naming an unknown
publisher. This is expected for unsigned open-source software and can appear
however you install it - a package manager removes the browser-download friction
but does not, on its own, guarantee the first-run prompt won't show. Click
**More info**, then **Run anyway**. To clear the "downloaded from the internet"
mark up front instead, run `Unblock-File .\odytty\odytty.exe` before launching.
Code-signed Windows binaries are a planned improvement.

OdyTTY can't yet be set as the Windows *default terminal* (the app Windows hands
console programs to when launched from Explorer or another program) - that needs
the Windows default-terminal handoff protocol, which OdyTTY doesn't implement
yet. Launch it directly instead: from the Start menu, by typing `odytty`, or
from a pinned shortcut. Detached/resumable session hosting is Unix-only in this
release; the Windows build opens local ConPTY-backed tabs and panes.

### Windows scope

The Windows build carries the full rendering, theme, effect, and inline-graphics
stack. It opens local ConPTY-backed tabs and panes, stores persistent
configuration under `%APPDATA%\odytty\`, discovers host fonts in
`C:\Windows\Fonts`, recognizes clickable drive-letter and UNC paths, opens
or reveals files through `cmd` and Explorer, and runs SSH connections inside
local pseudoconsole-backed tabs.

Detached and resumable session hosting, detached SSH, and headless
`--interactive` mode remain Unix-only. The full Open With application list is
not available on Windows, and the hostname field and command-palette shell
history currently degrade to empty. Interactive behavior is verified manually
on Windows devices; the blocking Windows CI leg proves the build compiles and
its unit tests pass.

### Updating

**Scoop (recommended):** refresh to the newest release. `scoop update` refreshes the bucket first so a just-published version is visible:

```powershell
scoop update
scoop update odytty
```

- **Portable zip:** re-download the always-latest `odytty-windows-x86_64.zip`, re-verify the hash against `SHA256SUMS`, and replace the old `odytty.exe`.

## Build From Source

From outside a source checkout, download and verify the always-latest release
archive first:

```sh
workdir=$(mktemp -d "${TMPDIR:-/tmp}/odytty-install.XXXXXX")
cd "$workdir"
curl -LO https://github.com/ghreprimand/odytty/releases/latest/download/odytty.tar.gz
curl -LO https://github.com/ghreprimand/odytty/releases/latest/download/SHA256SUMS
grep " odytty.tar.gz$" SHA256SUMS | sha256sum -c -
tar -xf odytty.tar.gz
cd odytty-*/
```

On macOS, use
`grep " odytty.tar.gz$" SHA256SUMS | shasum -a 256 -c -` for the verification
line.

Build the release binary:

```sh
cargo build --release --locked
```

Run from the source tree:

```sh
cargo run --release
```

After installation, plain `odytty` opens the native terminal. To print a
parser/core smoke transcript and exit:

```sh
odytty --core-smoke
```

Check the installed version:

```sh
odytty --version
```

Run a command directly inside OdyTTY:

```sh
odytty -e btop
odytty --working-directory /tmp -e sh -lc 'pwd; exec "$SHELL"'
odytty --title Monitor -e btop
```

## Command-Line Surface

Theming, fonts, and configuration are primarily driven by OdyTTY's in-app menus
(the settings panel, theme/font pickers, and the `Ctrl+Shift+P` command palette),
which change things live with a preview. These introspection commands are the
scriptable alternative - they print a snapshot and exit, which is handy for
verifying an install, automation, or wiring launchers:

```sh
odytty --version        # print the installed version
odytty --list-themes    # list built-in themes
odytty --list-fonts     # list discoverable monospace fonts
odytty --show-config    # print the effective configuration
odytty --core-smoke     # print a parser/core smoke transcript
```

`--list-themes` prints the 142 built-in themes as stable
`name`/`appearance`/`family` rows. `--list-fonts` prints discoverable
system font files. `--show-config` prints the stable effective-config subset,
including `symbol_fallback` and the resolved `symbol_font_source` fallback
chain. See [the settings authority](runtime-knobs.md) for every key, default,
range, and environment variable.

OdyTTY also hosts detached sessions that outlive the window:

```sh
odytty new --detached -e btop     # start a detached session, prints id=<id>
odytty list                       # list live detached sessions
odytty attach                     # reattach the only live session (or list choices)
odytty attach <id>                # reattach a specific session in a native window
odytty attach --diagnostic <id>   # print one status line without attaching
```

On non-macOS systems, detached sessions require `XDG_RUNTIME_DIR` to be set;
their owner-private sockets live under `$XDG_RUNTIME_DIR/odytty/`. macOS falls
back to the system temporary directory. The
[detached-session CLI reference](runtime-knobs.md#detached-session-cli) covers
metadata-only listings, reattachment, snapshot streaming, failure behavior,
bounded scrollback, socket privacy, and the idle timeout.

## Configuration Files

OdyTTY reads an optional config file at:

```text
$XDG_CONFIG_HOME/odytty/odytty.conf
```

falling back to `$HOME/.config/odytty/odytty.conf` when `XDG_CONFIG_HOME` is
unset. User themes are loaded from the theme directory alongside it:

```text
$XDG_CONFIG_HOME/odytty/themes
```

The file is line-based `key = value`; environment variables set at startup
override matching keys. See `docs/runtime-knobs.md` for every key, default, and
range, `docs/keybindings.md` for the keyboard reference, and
`docs/accessibility.md` for color-vision-deficiency modes, the minimum-contrast
floor, and the bell.

## User-Local Install

This is useful for quick testing on any Linux system:

```sh
cargo build --release --locked
install -Dm755 target/release/odytty "$HOME/.local/bin/odytty"
install -Dm644 dist/linux/io.unfinished_works.odytty.desktop \
  "$HOME/.local/share/applications/io.unfinished_works.odytty.desktop"
install -Dm644 dist/linux/io.unfinished_works.odytty.metainfo.xml \
  "$HOME/.local/share/metainfo/io.unfinished_works.odytty.metainfo.xml"
install -d "$HOME/.local/share/icons/hicolor"
cp -a dist/icons/hicolor/* "$HOME/.local/share/icons/hicolor/"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
gtk-update-icon-cache "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
```

After this, `odytty` should work from a shell and OdyTTY should appear in app
launchers that read Freedesktop desktop entries.

## System Install

For a non-packaged system-wide install:

```sh
cargo build --release --locked
sudo install -Dm755 target/release/odytty /usr/bin/odytty
sudo install -Dm644 dist/linux/io.unfinished_works.odytty.desktop \
  /usr/share/applications/io.unfinished_works.odytty.desktop
sudo install -Dm644 dist/linux/io.unfinished_works.odytty.metainfo.xml \
  /usr/share/metainfo/io.unfinished_works.odytty.metainfo.xml
sudo install -d /usr/share/icons/hicolor
sudo cp -a dist/icons/hicolor/* /usr/share/icons/hicolor/
sudo update-desktop-database /usr/share/applications 2>/dev/null || true
sudo gtk-update-icon-cache /usr/share/icons/hicolor 2>/dev/null || true
```

Prefer a real package for machines where installed-file ownership matters.

## Generic Versioned User Install

On any Linux system, a user can keep versioned OdyTTY builds without root by
installing each release under a versioned directory and pointing
`~/.local/bin/odytty` at the selected version:

```sh
version=0.8.2
cargo build --release --locked
install -Dm755 target/release/odytty \
  "$HOME/.local/opt/odytty/$version/bin/odytty"
ln -sfn "$HOME/.local/opt/odytty/$version/bin/odytty" \
  "$HOME/.local/bin/odytty"
install -Dm644 dist/linux/io.unfinished_works.odytty.desktop \
  "$HOME/.local/share/applications/io.unfinished_works.odytty.desktop"
install -Dm644 dist/linux/io.unfinished_works.odytty.metainfo.xml \
  "$HOME/.local/share/metainfo/io.unfinished_works.odytty.metainfo.xml"
install -d "$HOME/.local/share/icons/hicolor"
cp -a dist/icons/hicolor/* "$HOME/.local/share/icons/hicolor/"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
gtk-update-icon-cache "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
odytty --version
```

To roll back, repoint the symlink to an older directory under
`~/.local/opt/odytty/`.

## Odyssey/LFS Versioned Install

Odyssey source builds are versioned by pacman, not by leaving build products in
the source tree. A release tag such as `v0.8.2` should be archived into
`/sources/odytty-0.8.2.tar.gz`, then built from `~/pkgbuilds/odytty/PKGBUILD`
with `odyssey-build`.

Example PKGBUILD:

```bash
# Maintainer: Unfinished Works <maintainers@odytty.unfinished-works.com>
pkgname=odytty
pkgver=0.8.2
pkgrel=1
pkgdesc="GPU-rendered Rust terminal emulator with an Odyssey visual identity"
arch=('x86_64')
url="https://github.com/ghreprimand/odytty"
license=('GPL-3.0-only')
depends=('fontconfig' 'freetype2' 'vulkan-icd-loader')
makedepends=('cargo' 'rust')
source=("file:///sources/${pkgname}-${pkgver}.tar.gz")
sha256sums=('SKIP')

build() {
    cd "${pkgname}-${pkgver}"
    cargo build --release --locked
}

package() {
    cd "${pkgname}-${pkgver}"

    install -Dm755 target/release/odytty "$pkgdir/usr/bin/odytty"
    install -Dm644 dist/linux/io.unfinished_works.odytty.desktop \
        "$pkgdir/usr/share/applications/io.unfinished_works.odytty.desktop"
    install -Dm644 dist/linux/io.unfinished_works.odytty.metainfo.xml \
        "$pkgdir/usr/share/metainfo/io.unfinished_works.odytty.metainfo.xml"
    install -d "$pkgdir/usr/share/icons"
    cp -a dist/icons/hicolor "$pkgdir/usr/share/icons/"
    install -Dm644 README.md "$pkgdir/usr/share/doc/$pkgname/README.md"
    install -Dm644 docs/install.md "$pkgdir/usr/share/doc/$pkgname/install.md"
    install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
```

Build and install:

```sh
cd ~/pkgbuilds/odytty
odyssey-build
```

Verify pacman ownership and version tracking:

```sh
pacman -Qi odytty
pacman -Qo /usr/bin/odytty \
  /usr/share/applications/io.unfinished_works.odytty.desktop
odytty --show-config
odytty --version
```

Use `pkgrel=1` for the first packaging revision of a source release. If the
source does not change but the PKGBUILD does, keep `pkgver` at the source
version and bump `pkgrel`.

## What The Desktop Entry Does

`dist/linux/io.unfinished_works.odytty.desktop` registers OdyTTY as a GUI
application in the `System` and `TerminalEmulator` categories:

```ini
Exec=odytty
Terminal=false
Categories=System;TerminalEmulator;
X-TerminalArgExec=-e
X-TerminalArgDir=--working-directory=
X-TerminalArgTitle=--title=
```

The entry uses the OdyTTY icon name. Packages should install the SVG and PNG
assets under the hicolor icon theme, for example:

```text
/usr/share/icons/hicolor/scalable/apps/io.unfinished_works.odytty.svg
/usr/share/icons/hicolor/256x256/apps/io.unfinished_works.odytty.png
```

`dist/linux/io.unfinished_works.odytty.metainfo.xml` gives AppStream-aware
tools a stable application id, license, summary, homepage, bug tracker, and
release version. It belongs in:

```text
/usr/share/metainfo/io.unfinished_works.odytty.metainfo.xml
```

## Terminfo

OdyTTY currently runs child shells with:

```text
TERM=xterm-256color
```

That is conservative and avoids requiring a custom terminfo database entry on
the local machine, remote SSH hosts, or `sudo` environments. Do not package an
OdyTTY-specific terminfo entry until the binary also starts using that `TERM`
value and the release documents remote-host setup.

## Default Terminal

Linux does not have one universal default-terminal mechanism. App launchers,
file managers, desktop environments, and scripts use different conventions.

### LFS Or Other Manual Systems

On a Linux From Scratch-style system, app launcher registration usually means
installing the binary and `.desktop` file as shown above. To make shell scripts
that respect `$TERMINAL` choose OdyTTY:

```sh
export TERMINAL=odytty
```

Put that in the environment setup used by the desktop session, not only an
interactive shell startup file, if graphical applications need to see it.

For a keyboard shortcut such as `Ctrl+Alt+T`, configure the shortcut in the
desktop environment or window manager to run:

```sh
odytty
```

### `xdg-terminal-exec`

Some systems use the proposed `xdg-terminal-exec` default-terminal mechanism.
That system selects terminal emulators from installed desktop entries and a
preference file such as:

```text
$HOME/.config/xdg-terminals.list
```

The desktop entry advertises OdyTTY's command execution arguments:

```ini
X-TerminalArgExec=-e
X-TerminalArgDir=--working-directory=
X-TerminalArgTitle=--title=
```

Then a user can prefer OdyTTY with:

```sh
printf 'io.unfinished_works.odytty.desktop\n' > "$HOME/.config/xdg-terminals.list"
```

### Debian-Style `x-terminal-emulator`

Debian-family systems often use `update-alternatives` for
`x-terminal-emulator`. A package can register OdyTTY as an alternative:

```sh
sudo update-alternatives --install \
  /usr/bin/x-terminal-emulator x-terminal-emulator /usr/bin/odytty 50
sudo update-alternatives --config x-terminal-emulator
```

On systems without `update-alternatives`, this mechanism does not exist unless
the distribution or local system owner installs it.

### macOS

macOS has no OS-level "default terminal" setting. Unlike the default browser
or mail app, macOS exposes no system preference to replace Terminal.app as the
terminal other applications hand console programs off to; Terminal.app remains
the system default. Launch OdyTTY directly instead - from **Launchpad**, from
**Spotlight** (⌘-Space, type "OdyTTY"), or from a **Dock** icon. A Homebrew
cask install places `OdyTTY.app` in `/Applications`, so it appears in Launchpad
and Spotlight automatically and can be dragged to the Dock to pin it.

Windows likewise cannot yet be set as the system default terminal - the
Windows section above explains why (the default-terminal handoff protocol is
not implemented). Launch it directly there too.

## Troubleshooting

### Slow rendering / software adapter

OdyTTY renders on the GPU through `wgpu` (Vulkan on Linux, Metal on macOS,
Direct3D 12 on Windows, GL as a fallback). If the terminal feels very slow even
with all visual effects turned off, the most common cause is that no hardware
GPU adapter was available and the graphics stack silently fell back to a
**software rasterizer**, which does all rendering on the CPU.

**How to check which adapter is in use.** Two ways report the same information:

- The **About panel** (open the command palette with `Ctrl+Shift+P` and select
  *About*, or the settings panel's About tab) shows the active renderer's
  adapter name, backend, and device class.
- On startup OdyTTY prints one line to stderr naming the adapter, e.g.
  `odytty: GPU adapter: llvmpipe (LLVM 17.0.6, 256 bits) (Vulkan, Cpu)`. When a
  software adapter is selected it also prints a warning:
  `odytty: WARNING: rendering in software (...); expect low performance`.

**What a software adapter means.** Names such as **llvmpipe** or **lavapipe**
(Mesa's software renderers), **SwiftShader** (Google's software renderer), or a
device class of **Cpu** indicate CPU-only rendering. On Windows, **Microsoft
Basic Render Driver** (WARP) is the equivalent software fallback. Software
rendering works correctly but is far slower than a real GPU - especially on
older hardware.

**Common fixes.**

- **Linux:** install the Vulkan driver for your GPU. On Debian/Ubuntu that is
  `mesa-vulkan-drivers` (plus `vulkan-tools` for `vulkaninfo`); on Arch it is
  `vulkan-icd-loader` together with the vendor package (`vulkan-radeon`,
  `vulkan-intel`, or `nvidia-utils`). Verify with `vulkaninfo | head` - if it
  reports only llvmpipe, the hardware ICD is still missing. Over remote/SSH or
  in a VM without GPU passthrough, software rendering may be the only option.
- **Windows:** install the latest GPU vendor driver (NVIDIA/AMD/Intel). WARP is
  usually selected only when no hardware driver is present, e.g. in a bare VM or
  a fresh install before drivers are added.
- **macOS:** Metal is always hardware-backed on supported machines; a software
  adapter here is unusual and typically indicates a virtualized environment.

## Public Release Direction

For an upstream release that avoids maintaining many distro-specific packages:

- publish source tarballs with `SHA256SUMS` (the Release workflow attaches both);
- create a GitHub Release for the tag so package monitors can track upstream
  versions;
- attach the x86_64 AppImage built by `dist/appimage/build-appimage.sh`;
- attach the unsigned Windows x86_64 zip built by the release workflow;
- keep the Scoop bucket manifest in sync with the release artifact names;
- publish the AUR `odytty` package from `dist/aur/` (see its README runbook);
- include `dist/linux/io.unfinished_works.odytty.desktop`;
- include `dist/linux/io.unfinished_works.odytty.metainfo.xml`;
- include `dist/icons/hicolor/`;
- include install docs for source builds, versioned user-local installs, and
  packagers;
- let other distro-specific `.deb`, `.rpm`, Nix, and similar packages be
  maintained downstream using the same install surface.
