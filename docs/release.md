# Releasing OdyTTY

Use this guide to cut a tagged OdyTTY release, verify its 15 published assets,
and confirm the Scoop, Homebrew, and AUR channels updated. Replace `X.Y.Z` with
the version being released.

## Contents

- [Continuous Integration](#continuous-integration)
- [Release Readiness](#release-readiness)
- [Release Artifacts](#release-artifacts)
- [Create A Release](#create-a-release)
- [Release Workflow Jobs](#release-workflow-jobs)
- [Verify Publishing Channels](#verify-publishing-channels)
- [Build A Fallback Source Archive](#build-a-fallback-source-archive)
- [Verify The Odyssey Package](#verify-the-odyssey-package)
- [Package-Monitor Compatibility](#package-monitor-compatibility)
- [Odyssey-Mon Upstream Tracking](#odyssey-mon-upstream-tracking)
- [Versioning](#versioning)

## Continuous Integration

OdyTTY uses GitHub-hosted runners for these workflows:

| Workflow | Trigger | Result |
| --- | --- | --- |
| `.github/workflows/ci.yml` | Pushes and pull requests to `master` | Formats, builds, lints, and tests on Ubuntu, macOS, and Windows; publishes no artifacts |
| `.github/workflows/release.yml` | `vX.Y.Z` tags or manual validation | Builds all seven release artifact types; tag runs also publish the release and update package channels |
| `.github/workflows/rustsec-audit.yml` | Pull requests touching `Cargo.lock`, `Cargo.toml`, or the audit script or workflow; a weekly schedule; and manual dispatch | Runs `cargo audit` against the locked dependency graph; the `release` job runs the same audit before publishing; publishes no artifacts |
| `.github/workflows/deep-fuzz.yml` | Weekly schedule or manual dispatch | Runs the ignored parser/protocol and graphics fuzz tiers at 40,000 iterations; publishes no artifacts |

Third-party workflow actions are pinned to reviewed commit SHAs rather than
floating tags.

The CI gate runs these commands on all supported platforms:

```sh
cargo fmt --check
cargo build --release --locked
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

The macOS leg runs the test command single-threaded as
`cargo test --locked -- --test-threads=1`, with a per-attempt timeout and one
automatic retry for the known runner PTY-teardown deadlock. A genuine test
failure still fails the job.

Release publishing never runs for fork pull requests. A normal release lands
the version commit on `master` and then pushes an annotated tag.

## Release Readiness

The release must agree with the current public references:

| Surface | Authoritative reference |
| --- | --- |
| Terminal and native-app behavior | [Feature Reference](features.md) |
| Default shortcuts and rebinding | [Keybindings](keybindings.md) |
| Settings, defaults, and environment variables | [Runtime Knobs](runtime-knobs.md) |
| Supported install paths and artifact names | [Install Guide](install.md) |

Linux, macOS, and Windows build from the published source archive and run in
the CI matrix. Releases also carry desktop metadata and hicolor icons for
Linux packages.

The upstream release does not currently publish Nix, Flatpak, or Snap packages.
A package must not silently change the user's default terminal; it should
register OdyTTY as available and leave selection to the user.

## Release Artifacts

Each release publishes seven artifact types under both an always-latest alias
and a byte-identical version-pinned name:

| Artifact | Always-latest alias | Version-pinned name |
| --- | --- | --- |
| Debian package | `odytty-amd64.deb` | `odytty-X.Y.Z-amd64.deb` |
| RPM package | `odytty-x86_64.rpm` | `odytty-X.Y.Z-x86_64.rpm` |
| Linux binary tarball | `odytty-linux-x86_64.tar.gz` | `odytty-X.Y.Z-linux-x86_64.tar.gz` |
| Linux AppImage | `odytty-x86_64.AppImage` | `odytty-X.Y.Z-x86_64.AppImage` |
| macOS Apple Silicon app zip | `odytty-macos-arm64.zip` | `odytty-X.Y.Z-macos-arm64.zip` |
| Windows portable zip | `odytty-windows-x86_64.zip` | `odytty-X.Y.Z-windows-x86_64.zip` |
| Source archive | `odytty.tar.gz` | `odytty-X.Y.Z.tar.gz` |

`SHA256SUMS` is the fifteenth asset. Each alias and its pinned twin have the
same hash because they contain the same bytes.

Durable download links use the aliases under `releases/latest/download/`.
Pinned names select one specific version. See the
[install artifact table](install.md#release-artifact-names-and-checksums) for
the user-facing download contract.

Release artifacts are not cryptographically signed. The macOS app is ad-hoc
signed during assembly, but it is not Developer ID signed or notarized.

## Create A Release

### 1. Update Release Metadata

Set `Cargo.toml` to `X.Y.Z` and refresh `Cargo.lock`. Keep the declared MSRV in
`Cargo.toml` aligned with `rust-toolchain.toml` if the Rust version changes.

Add the newest `<release>` entry to
`dist/linux/io.unfinished_works.odytty.metainfo.xml`. Add the release headline
to `DEVLOG.md` using this shape:

```text
## YYYY-MM-DD -- Release vX.Y.Z
```

Commit these changes together, push `master`, and wait for all three CI
platform jobs to pass.

### 2. Run Local Release Checks

Run the full suite rather than `cargo test --lib`. CLI and attach integration
tests require the compiled `odytty` binary.

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --release --locked
target/release/odytty --version
desktop-file-validate dist/linux/io.unfinished_works.odytty.desktop
appstreamcli validate --pedantic dist/linux/io.unfinished_works.odytty.metainfo.xml
```

The tag workflow smoke-tests the Windows and macOS binaries before packaging,
the assembled AppImage, and the binary inside the Linux tarball staging tree.
The `.deb` and `.rpm` packages receive metadata and file-list validation but are
not executed.

### 3. Push The Annotated Tag

```sh
version=X.Y.Z
git tag -a "v${version}" -m "OdyTTY v${version}"
git push origin "v${version}"
```

The tag starts all producer jobs, publishes the GitHub Release, and then runs
the three package-channel jobs.

### 4. Verify The Published Release

Confirm the release has 15 assets: seven aliases, seven pinned copies, and
`SHA256SUMS`. Verify that every alias/pinned pair has matching hashes.

Download the pinned source archive and confirm it builds:

```sh
cargo build --release --locked
```

Also confirm the release title, tag, `Cargo.toml` version, and metainfo release
entry all use `X.Y.Z`.

## Release Workflow Jobs

`release.yml` has seven producer jobs and four tag-only publishing jobs:

| Job | Artifact or channel | Guard |
| --- | --- | --- |
| `source` | Source tarball | Runs for tag and manual validation |
| `appimage` | Linux AppImage | Runs for tag and manual validation |
| `linux-tarball` | Standalone Linux binary tarball | Runs for tag and manual validation |
| `deb` | Debian package | Runs for tag and manual validation |
| `rpm` | RPM package | Runs for tag and manual validation |
| `windows` | Windows portable zip | Runs for tag and manual validation |
| `macos` | Ad-hoc-signed macOS app zip | Runs for tag and manual validation |
| `release` | GitHub Release, aliases, pinned copies, and `SHA256SUMS` | Tag only; requires all seven producers |
| `scoop` | In-repo Scoop manifest | Tag only; runs after `release` and pushes with `GITHUB_TOKEN` |
| `homebrew` | External Homebrew tap | Tag only; runs after `release`; validates locally and publishes when `HOMEBREW_TAP_DEPLOY_KEY` is present |
| `aur` | AUR package | Tag only; runs after `release` through `aur-publish.yml`; publishes when `AUR_SSH_PRIVATE_KEY` is present |

The Homebrew and AUR credentials are configured for the live channels. Their
missing-key paths remain clean validation-only no-ops so forks and replacement
repositories can use the workflow safely.

## Verify Publishing Channels

After `release` finishes, verify every channel:

| Channel | Expected automatic result | Fallback |
| --- | --- | --- |
| Scoop | `bucket/odytty.json` is committed to `master` with the new version, Windows zip URL, and hash | Update those three fields from the published `SHA256SUMS` |
| Homebrew | The cask and source formula are stamped and pushed to `ghreprimand/homebrew-odytty` | Follow the [Homebrew publishing guide](../dist/homebrew/README.md) |
| AUR | The `odytty` package is stamped and pushed to the AUR | Follow the [AUR publishing guide](../dist/aur/README.md) |

For Scoop, the client reads the pinned `version`, `url`, and `hash`. The
`autoupdate` block is metadata for maintainer tooling and does not update
installed clients by itself.

The Scoop hash comes from the
`odytty-X.Y.Z-windows-x86_64.zip` row in `SHA256SUMS`. The Homebrew cask uses
the macOS zip row, while the formula and AUR package use the source-tarball row.

## Build A Fallback Source Archive

The workflow normally builds the archive. To create the same versioned tarball
locally when GitHub Actions is unavailable:

```sh
version=X.Y.Z
git archive --format=tar.gz --prefix="odytty-${version}/" \
  -o "odytty-${version}.tar.gz" "v${version}"
sha256sum "odytty-${version}.tar.gz" > SHA256SUMS
```

## Verify The Odyssey Package

Build the Odyssey package from the published archive rather than the working
tree. Then verify ownership and the installed version:

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

The GitHub Release is the upstream signal for package monitors. These values
must agree:

```text
tag: vX.Y.Z
release title: vX.Y.Z
Cargo.toml version: X.Y.Z
archive: odytty-X.Y.Z.tar.gz
```

Do not publish a release whose source archive fails:

```sh
cargo build --release --locked
```

## Odyssey-Mon Upstream Tracking

Odyssey-Mon reads the installed package version from `pacman -Qi odytty` and
compares it with GitHub tags. Configure the upstream source as:

```text
type: github
owner: ghreprimand
repo: odytty
tag_prefix: v
```

Odyssey-Mon normalizes the leading `v` and the package-release suffix, so an
upstream `vX.Y.Z` tag compares with an installed `X.Y.Z-1` package.

## Versioning

Use semantic versions for source releases:

```text
v0.1.0
...
v0.1.9
v0.2.0
```

For recipe-only packaging changes, keep the source version and increment the
downstream package release field, such as `pkgrel=2` in a PKGBUILD.
