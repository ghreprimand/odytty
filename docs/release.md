# OdyTTY Release Process

OdyTTY releases create both a source tag and a GitHub Release entry that package
monitors can discover. The first public release was `v0.1.0`; the current
release is `v0.7.1`. `Cargo.toml` must match the release version. The version
examples below use `0.6.2` as an illustrative tag — substitute the tag you are
cutting.

OdyTTY publishes a source tarball, a best-effort Linux x86_64 AppImage, an
unsigned Windows x86_64 portable zip, and `SHA256SUMS`. The same source tarball
builds on Linux, macOS, and Windows with `cargo build --release --locked`.

## Continuous Integration

Two GitHub Actions workflows back the process. Neither uses a self-hosted
runner; both run on standard GitHub-hosted runners, which are free with no quota
for public repositories.

- **`.github/workflows/ci.yml`** — on every push and pull request to `master`,
  builds and tests on `ubuntu-latest`, `macos-latest`, and `windows-latest`
  (`cargo fmt --check`, `cargo build --release --locked`, `cargo clippy
  --all-targets --locked -- -D warnings`, `cargo test --locked`). This is the
  cross-platform correctness gate; it produces no artifacts.
- **`.github/workflows/release.yml`** — on a `vX.Y.Z` tag push, creates the
  source tarball, Linux AppImage, Windows zip, `SHA256SUMS`, and publishes the
  GitHub Release automatically. Tag pushes are restricted to maintainers, so
  this never runs from fork pull requests.

Because the release workflow does the archive, checksum, and publish steps, a
normal release is: land the version bump on `master`, then push the tag. The
manual archive commands below are documented only as a fallback for building a
tarball locally (for example, to feed the Odyssey package before the tag's CI
run finishes).

## Current Release Readiness

The current release ships:

- plain `odytty` launches the native terminal;
- `odytty -e command args...` executes a command directly in the initial PTY;
- `--working-directory DIR` sets the initial shell/command directory;
- `--title TITLE` sets the initial window title;
- `odytty --version`, `--help`, `--list-themes`, `--list-fonts`, and
  `--show-config` work without opening a window;
- `odytty new` / `list` / `attach [ID]` manage detached sessions on Unix, with
  an in-window Manage Sessions overlay and a Detach & switch action;
- tabs and split panes (first split via `Ctrl+Shift+E` / `Ctrl+Shift+O`, then
  the `Ctrl-b` prefix model);
- overlays for the command palette, connection manager, session replay, theme
  picker, and theme builder;
- 100 built-in themes with a perceptual minimum-contrast floor and optional
  color-vision-deficiency modes;
- interactive/clickable paths (off by default) with `Ctrl+click`-to-open and an
  in-app image lightbox;
- a mouse-cursor shape over the grid (I-beam), hyperlinks (hand), and chrome
  (arrow);
- desktop launcher metadata in `dist/linux/`;
- hicolor SVG and PNG icon assets in `dist/icons/hicolor/`;
- source, AppImage, Windows zip, user-local, system, Odyssey/LFS, and downstream packaging
  instructions are documented;
- Linux, macOS, and Windows build from the published source archive and are
  exercised in CI.

Deferred:

- signed Windows installers, MSI/NSIS/WiX, and public `.pdb` uploads;
- `.dmg` / signed or notarized macOS binary artifacts;
- `.deb`, `.rpm`, Nix, Flatpak, Snap, or AUR packages maintained upstream;
- package-managed Debian `x-terminal-emulator` registration;
- a custom `TERM` value and matching OdyTTY terminfo entry.

A release must not silently change a user's default terminal; packages should
register OdyTTY as available and let the user choose it.

## Release Artifacts

A release includes exactly:

```text
odytty-0.6.2.tar.gz
odytty.tar.gz
odytty-0.6.2-x86_64.AppImage
odytty-x86_64.AppImage
odytty-0.6.2-windows-x86_64.zip
odytty-windows-x86_64.zip
SHA256SUMS
```

These are produced and attached by `release.yml`. The version-less files are
always-latest aliases for durable download URLs; each alias and its
version-pinned twin are byte-identical and therefore show matching hashes in
`SHA256SUMS`. Detached signatures may be added later if upstream signing is set
up; CI does not sign today.

## Create A Release

1. Verify the version in `Cargo.toml` matches the intended tag, and refresh
   `Cargo.lock` (`cargo build`).

2. Run the release checks locally (CI runs the same on all supported platforms):

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --release --locked
target/release/odytty --version
desktop-file-validate dist/linux/io.unfinished_works.odytty.desktop
appstreamcli validate --pedantic dist/linux/io.unfinished_works.odytty.metainfo.xml
```

   The `clippy` line mirrors CI's `-D warnings` gate. Run the full
   `cargo test --locked` (not `cargo test --lib`): the CLI/attach integration
   tests need the compiled `odytty` binary, which `--lib` does not build.
   Windows release zip contents are verified by the tag-triggered release
   workflow's `odytty.exe --version` smoke test.

3. Update `dist/linux/io.unfinished_works.odytty.metainfo.xml` to add a
   `<release>` entry for the new version (newest first) and add the headline
   `DEVLOG.md` entry with a `## YYYY-MM-DD -- Release vX.Y.Z` heading. Commit the
   version bump together with these and any other release-note/doc updates, then
   push `master`. Confirm the `ci.yml` run is green on Linux, macOS, and
   Windows.

4. Tag the release and push the tag. This triggers `release.yml`, which builds
   the tarball, AppImage, Windows zip, writes `SHA256SUMS`, and publishes the
   GitHub Release:

```sh
git tag -a v0.6.2 -m "OdyTTY v0.6.2"
git push origin v0.6.2
```

5. Confirm the published release has exactly the artifact set listed above and
   that `SHA256SUMS` includes matching hashes for each alias/version-pinned pair.
   Confirm the source archive builds with `cargo build --release --locked`.

   To produce the same tarball locally without CI (fallback):

```sh
git archive --format=tar.gz --prefix=odytty-0.6.2/ \
  -o odytty-0.6.2.tar.gz v0.6.2
sha256sum odytty-0.6.2.tar.gz > SHA256SUMS
```

6. For Windows distribution metadata, the Scoop bucket manifest
   (`bucket/odytty.json`) must be bumped to the new release — its pinned
   `version`, `url`, and `hash` are the only fields the Scoop **client** reads,
   so `scoop update odytty` serves the previous version until this bump lands.
   (The manifest's `autoupdate` block is **not** client-side: it is metadata for
   maintainer tooling — checkver scripts and Excavator bots — and does not keep
   installed clients current on its own.) A manifest bump is therefore
   **required every release**, and it can only happen **after** the release's
   `SHA256SUMS` asset exists (the hash comes from the
   `odytty-<version>-windows-x86_64.zip` row).

   **Primary path — automatic (CI).** The `scoop` job in `release.yml` runs
   after the release is published: it downloads `SHA256SUMS`, rewrites
   `bucket/odytty.json` (version/url/hash), and commits `Scoop: bump manifest to
   vX.Y.Z` to `master`. After tagging, confirm this commit landed on `master`
   shortly after the release finishes publishing, and that the manifest's pinned
   `version` matches the tag. No manual edit is needed when the job succeeds.

   **Fallback — manual.** If the `scoop` job failed or was skipped (for example,
   branch protection blocks `GITHUB_TOKEN` pushes to `master`), bump the
   manifest by hand: set `version` to the release, set the `64bit` `url` to
   `…/releases/download/v<version>/odytty-<version>-windows-x86_64.zip`, and set
   `hash` to that file's row in the published `SHA256SUMS`; commit and push to
   `master`. The manifest first became installable from this repo with the
   release that included `odytty-<version>-windows-x86_64.zip` and `SHA256SUMS`.

7. Build the Odyssey package from the published archive, not from the working
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
tag: v0.6.2
release title: v0.6.2
Cargo.toml version: 0.6.2
archive: odytty-0.6.2.tar.gz
```

Do not publish a release entry for a tag whose source archive cannot be built
with `cargo build --release --locked`.

## Odyssey-Mon Upstream Tracking

After OdyTTY is installed as a local pacman package, Odyssey-Mon sees the local
installed version from `pacman -Qi odytty`, for example `0.6.2-1`.

Configure upstream tracking as a GitHub source:

```text
type: github
owner: ghreprimand
repo: odytty
tag_prefix: v
```

With that mapping, upstream releases such as `v0.6.2` can be compared against
the installed pacman version `0.6.2-1`, after Odyssey-Mon normalizes the `v`
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
