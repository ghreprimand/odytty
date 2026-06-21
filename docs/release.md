# OdyTTY Release Process

OdyTTY releases create both a source tag and a GitHub Release entry that package
monitors can discover. The first public release was `v0.1.0`; the current
release is `v0.2.1`, which fixes insert/replace mode (IRM) so editors like
macOS pico/nano no longer overwrite text instead of inserting it, bounds
scrollback memory by default, and makes the test suite deterministic.
`Cargo.toml` must match the release version.

OdyTTY ships as **source only**. A release publishes a source tarball plus
`SHA256SUMS`; there is no prebuilt binary or disk image for any platform. The
same tarball builds on Linux and macOS with `cargo build --release --locked`.

## Continuous Integration

Two GitHub Actions workflows back the process. Neither uses a self-hosted
runner; both run on standard GitHub-hosted runners, which are free with no quota
for public repositories.

- **`.github/workflows/ci.yml`** — on every push and pull request to `master`,
  builds and tests on `ubuntu-latest` and `macos-latest` (`cargo fmt --check`,
  `cargo build --release --locked`, `cargo clippy --all-targets --locked -- -D
  warnings`, `cargo test --locked`). This is the cross-platform correctness
  gate; it produces no artifacts.
- **`.github/workflows/release.yml`** — on a `vX.Y.Z` tag push, creates the
  source tarball, computes `SHA256SUMS`, and publishes the GitHub Release
  automatically. Tag pushes are restricted to maintainers, so this never runs
  from fork pull requests.

Because the release workflow does the archive, checksum, and publish steps, a
normal release is: land the version bump on `master`, then push the tag. The
manual archive commands below are documented only as a fallback for building a
tarball locally (for example, to feed the Odyssey package before the tag's CI
run finishes).

## Current Release Readiness

`v0.2.1` ships:

- plain `odytty` launches the native terminal;
- `odytty -e command args...` executes a command directly in the initial PTY;
- `--working-directory DIR` sets the initial shell/command directory;
- `--title TITLE` sets the initial window title;
- `odytty --version`, `--help`, `--list-themes`, `--list-fonts`, and
  `--show-config` work without opening a window;
- a mouse-cursor shape over the grid (I-beam), hyperlinks (hand), and chrome
  (arrow);
- desktop launcher metadata in `dist/linux/`;
- hicolor SVG and PNG icon assets in `dist/icons/hicolor/`;
- source, user-local, system, Odyssey/LFS, and downstream packaging
  instructions are documented;
- Linux and macOS both build from the published source archive and are
  exercised in CI.

Deferred:

- binary AppImage / `.dmg` / packaged artifacts of any kind (source-only by
  design);
- `.deb`, `.rpm`, Nix, Flatpak, Snap, or AUR packages maintained upstream;
- package-managed Debian `x-terminal-emulator` registration;
- a custom `TERM` value and matching OdyTTY terminfo entry.

A release must not silently change a user's default terminal; packages should
register OdyTTY as available and let the user choose it.

## Release Artifacts

A release includes exactly:

```text
odytty-0.2.0.tar.gz
SHA256SUMS
```

These are produced and attached by `release.yml`. (Detached signatures such as
`odytty-0.2.0.tar.gz.minisig` may be added later if upstream signing is set up;
CI does not sign today.)

## Create A Release

1. Verify the version in `Cargo.toml` matches the intended tag, and refresh
   `Cargo.lock` (`cargo build`).

2. Run the release checks locally (CI runs the same on both platforms):

```sh
cargo fmt --check
cargo check --locked
cargo test --locked
cargo build --release --locked
target/release/odytty --version
desktop-file-validate dist/linux/io.unfinished_works.odytty.desktop
appstreamcli validate --pedantic dist/linux/io.unfinished_works.odytty.metainfo.xml
```

3. Commit the version bump and any release-note/doc updates, then push `master`.
   Confirm the `ci.yml` run is green on both Linux and macOS.

4. Tag the release and push the tag. This triggers `release.yml`, which builds
   the tarball, writes `SHA256SUMS`, and publishes the GitHub Release:

```sh
git tag -a v0.2.0 -m "OdyTTY v0.2.0"
git push origin v0.2.0
```

5. Confirm the published release has exactly `odytty-0.2.0.tar.gz` and
   `SHA256SUMS`, and that the archive builds with `cargo build --release
   --locked`.

   To produce the same tarball locally without CI (fallback):

```sh
git archive --format=tar.gz --prefix=odytty-0.2.0/ \
  -o odytty-0.2.0.tar.gz v0.2.0
sha256sum odytty-0.2.0.tar.gz > SHA256SUMS
```

6. Build the Odyssey package from the published archive, not from the working
   tree, and verify file ownership:

```sh
cd ~/pkgbuilds/odytty
odyssey-build
pacman -Qi odytty
pacman -Qo /usr/bin/odytty \
  /usr/share/applications/io.unfinished_works.odytty.desktop \
  /usr/share/metainfo/io.unfinished_works.odytty.metainfo.xml
odytty --version
```

## Package-Monitor Compatibility

The GitHub Release is the upstream signal for package monitors. The tag,
release title, source archive, checksum file, and `Cargo.toml` version should
all agree:

```text
tag: v0.2.0
release title: v0.2.0
Cargo.toml version: 0.2.0
archive: odytty-0.2.0.tar.gz
```

Do not publish a release entry for a tag whose source archive cannot be built
with `cargo build --release --locked`.

## Odyssey-Mon Upstream Tracking

After OdyTTY is installed as a local pacman package, Odyssey-Mon sees the local
installed version from `pacman -Qi odytty`, for example `0.2.0-1`.

Configure upstream tracking as a GitHub source:

```text
type: github
owner: ghreprimand
repo: odytty
tag_prefix: v
```

With that mapping, upstream releases such as `v0.2.0` can be compared against
the installed pacman version `0.2.0-1`, after Odyssey-Mon normalizes the `v`
prefix and package-release suffix.

## Versioning

Use semantic versions for source releases:

```text
v0.1.0
...
v0.1.9
v0.2.0
```

For package-only recipe changes, keep the source version and bump the package
release field in the downstream package recipe, such as `pkgrel=2` in a
PKGBUILD.
