# Installing OdyTTY

OdyTTY ships as a versioned release. Each release provides:

- a git tag (`vX.Y.Z`) and source tarball;
- a GitHub Release entry with `SHA256SUMS` for every artifact;
- a best-effort x86_64 **AppImage** (one download for most Linux distributions);
- an unsigned Windows x86_64 portable **zip**;
- a prebuilt **macOS** `.app` zip for Apple Silicon (ad-hoc signed) plus a Homebrew tap;
- an **AUR** package (`odytty`) for Arch-family systems;
- source-build instructions for Odyssey/LFS and other developer systems;
- a desktop entry, AppStream metadata, and icon installed into Freedesktop
  locations.

Pick by system: Scoop (or a direct zip download) on Windows, Homebrew on
macOS, AppImage for a no-install single-file Linux run, the AUR package on
Arch-family systems, and a pacman-tracked source package on Odyssey itself so
the install is versioned, owned, removable, and visible to Odyssey-Mon.

## Windows

Windows support is new and still maturing — it builds and runs the full
terminal and is exercised on a Windows CI leg every push, but the polish bar is
behind Linux. Bug reports for the Windows build are especially welcome; open an
issue with your Windows version and a short repro.

The Windows release is an unsigned portable `odytty.exe` inside
`odytty-windows-x86_64.zip`. There is no installer, and nothing is written
outside your profile — configuration lives under `%APPDATA%\odytty\`. Scoop is
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
`scoop update odytty` reads. From your side there is nothing to do per release —
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
however you install it — a package manager removes the browser-download friction
but does not, on its own, guarantee the first-run prompt won't show. Click
**More info**, then **Run anyway**. To clear the "downloaded from the internet"
mark up front instead, run `Unblock-File .\odytty\odytty.exe` before launching.
Code-signed Windows binaries are a planned improvement.

OdyTTY can't yet be set as the Windows *default terminal* (the app Windows hands
console programs to when launched from Explorer or another program) — that needs
the Windows default-terminal handoff protocol, which OdyTTY doesn't implement
yet. Launch it directly instead: from the Start menu, by typing `odytty`, or
from a pinned shortcut. Detached/resumable session hosting is Unix-only in this
release; the Windows build opens local ConPTY-backed tabs and panes.

## macOS (Apple Silicon)

macOS builds and passes the full test suite on every push, and the release
ships a prebuilt `OdyTTY.app` for Apple Silicon (arm64). The binary is
**ad-hoc code-signed** in CI (`codesign -s -`), which needs no Apple Developer
account and no notarization — enough for the app to launch, but not an Apple
Developer identity. There is no `.dmg` and no Gatekeeper-approved signature yet.

### Homebrew (recommended)

The Homebrew tap is the least-friction path and needs no manual quarantine
handling. Add the tap and install the cask:

```sh
brew tap ghreprimand/odytty
brew install --cask odytty
```

The **cask** installs the prebuilt, ad-hoc-signed `OdyTTY.app` into
`/Applications`. Homebrew is Apple-independent, and `brew` strips the
quarantine attribute on install, so the app launches with no "unidentified
developer" Gatekeeper warning even though it is not notarized. The cask
recipe goes live with the release it points at.

Prefer to compile locally instead? Use the **source formula**, which builds
from the release tarball with your own toolchain:

```sh
brew install ghreprimand/odytty/odytty
```

Per release there is nothing to do on your side: CI bumps the tap's cask to
each new version automatically shortly after the release publishes, so
`brew upgrade` picks it up.

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
ad-hoc-signed app past Gatekeeper without notarization. Homebrew installs do
this for you, so the step is only needed for a direct (non-brew) zip download.

### Build from source

A source build works today on macOS with the standard Rust toolchain:

```sh
cargo build --release
```

The resulting binary runs directly; the `.app` bundle and ad-hoc signature are
only assembled for the packaged release artifact.

### Signed / notarized builds

A signed, notarized `.dmg` (no `xattr` stopgap, no first-run friction on any
download path) requires enrollment in the Apple Developer Program. That is
deferred to if/when the project opts into that program; until then the ad-hoc
signature plus Homebrew's quarantine strip is the account-free path.

## AppImage (x86_64)

Download `odytty-<version>-x86_64.AppImage` from the GitHub Release, verify it
against `SHA256SUMS`, mark it executable, and run it:

```sh
sha256sum -c SHA256SUMS --ignore-missing
chmod +x odytty-*-x86_64.AppImage
./odytty-*-x86_64.AppImage
```

The AppImage bundles OdyTTY's own dependencies but **not** the graphics driver:
it uses the host's Vulkan ICD (Mesa or a vendor driver), so the machine needs a
working Vulkan setup — the same requirement as a source build. It is built on
the oldest supported Ubuntu LTS for a wide glibc floor. This is a best-effort
artifact; if Vulkan initialization fails on your host, build from source.

To integrate it into menus, tools like [Gear Lever][gearlever] or
`appimaged` register the bundled desktop entry and icon. The AppImage can be
built locally with `dist/appimage/build-appimage.sh`.

[gearlever]: https://github.com/mijorus/gearlever

## Arch Linux (AUR)

Install the `odytty` package with an AUR helper:

```sh
paru -S odytty      # or: yay -S odytty
```

…or manually:

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
depends on its maintainer. To pick up the very latest immediately regardless of
the AUR package's state, build from the release source tarball directly (the
PKGBUILD template and its publish runbook live in `dist/aur/`), or use the
always-latest download links below.

## Build From Source

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
scriptable alternative — they print a snapshot and exit, which is handy for
verifying an install, automation, or wiring launchers:

```sh
odytty --version        # print the installed version
odytty --list-themes    # list built-in themes
odytty --list-fonts     # list discoverable monospace fonts
odytty --show-config    # print the effective configuration
odytty --core-smoke     # print a parser/core smoke transcript
```

OdyTTY also hosts detached sessions that outlive the window:

```sh
odytty new -e btop                # start a detached session, prints id=<id>
odytty list                       # list live detached sessions
odytty attach                     # reattach the only live session (or list choices)
odytty attach <id>                # reattach a specific session in a native window
odytty attach --diagnostic <id>   # print one status line without attaching
```

On non-macOS systems, detached sessions require `XDG_RUNTIME_DIR` to be set;
their owner-private sockets live under `$XDG_RUNTIME_DIR/odytty/`. macOS falls
back to the system temporary directory.

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
version=0.8.1
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
the source tree. A release tag such as `v0.8.1` should be archived into
`/sources/odytty-0.8.1.tar.gz`, then built from `~/pkgbuilds/odytty/PKGBUILD`
with `odyssey-build`.

Example PKGBUILD:

```bash
# Maintainer: Unfinished Works <maintainers@odytty.unfinished-works.com>
pkgname=odytty
pkgver=0.8.1
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
rendering works correctly but is far slower than a real GPU — especially on
older hardware.

**Common fixes.**

- **Linux:** install the Vulkan driver for your GPU. On Debian/Ubuntu that is
  `mesa-vulkan-drivers` (plus `vulkan-tools` for `vulkaninfo`); on Arch it is
  `vulkan-icd-loader` together with the vendor package (`vulkan-radeon`,
  `vulkan-intel`, or `nvidia-utils`). Verify with `vulkaninfo | head` — if it
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
