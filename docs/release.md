# OdyTTY Release Process

OdyTTY releases should create both a source tag and a release entry that package
monitors can discover. The first public release was `v0.1.0`; the current
release target is `v0.1.2` because it fixes a native-session shutdown crash
seen in the default-terminal launch path. `Cargo.toml` should match the release
version.

## Current Release Readiness

Ready for `v0.1.2`:

- plain `odytty` launches the native terminal;
- `odytty --version`, `--help`, `--list-themes`, `--list-fonts`, and
  `--show-config` work without opening a window;
- desktop launcher metadata lives in `dist/linux/`;
- hicolor SVG and PNG icon assets live in `dist/icons/hicolor/`;
- source, user-local, system, Odyssey/LFS, and downstream packaging
  instructions are documented.

Deferred until after `v0.1.2`:

- binary AppImage artifacts;
- `.deb`, `.rpm`, Nix, Flatpak, Snap, or AUR packages maintained upstream;
- `odytty -e command args...`, `--working-directory`, and `--title`;
- `xdg-terminal-exec` and Debian `x-terminal-emulator` registration;
- a custom `TERM` value and matching OdyTTY terminfo entry.

The first release can be a source release plus integration files. It should not
claim full default-terminal compatibility until `-e` support exists.

## Release Artifacts

A minimal release should include:

```text
odytty-0.1.2.tar.gz
SHA256SUMS
```

Recommended once signing is set up:

```text
odytty-0.1.2.tar.gz.minisig
```

Later binary releases may add:

```text
odytty-x86_64.AppImage
odytty-aarch64.AppImage
```

## Create A Release

1. Verify the version in `Cargo.toml`.
2. Run the release checks:

```sh
cargo fmt --check
cargo check --locked
cargo test --lib --locked
cargo build --release --locked
target/release/odytty --version
desktop-file-validate dist/linux/io.unfinished_works.odytty.desktop
appstreamcli validate --pedantic dist/linux/io.unfinished_works.odytty.metainfo.xml
```

3. Tag the release:

```sh
git tag -a v0.1.2 -m "OdyTTY v0.1.2"
```

4. Create a source archive from the tag:

```sh
git archive --format=tar.gz --prefix=odytty-0.1.2/ \
  -o odytty-0.1.2.tar.gz v0.1.2
sha256sum odytty-0.1.2.tar.gz > SHA256SUMS
```

5. Publish a GitHub Release named `v0.1.2` and attach the archive and
   `SHA256SUMS`. Use the project-generated archive above for packaging
   instructions instead of relying on GitHub's auto-generated source archives.

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
tag: v0.1.2
release title: v0.1.2
Cargo.toml version: 0.1.2
archive: odytty-0.1.2.tar.gz
```

Do not publish a release entry for a tag whose source archive cannot be built
with `cargo build --release --locked`.

## Odyssey-Mon Upstream Tracking

After OdyTTY is installed as a local pacman package, Odyssey-Mon sees the local
installed version from `pacman -Qi odytty`, for example `0.1.2-1`.

Configure upstream tracking as a GitHub source:

```text
type: github
owner: ghreprimand
repo: odytty
tag_prefix: v
```

With that mapping, upstream releases such as `v0.1.2` can be compared against
the installed pacman version `0.1.2-1`, after Odyssey-Mon normalizes the `v`
prefix and package-release suffix.

## Versioning

Use semantic versions for source releases:

```text
v0.1.0
v0.1.1
v0.1.2
v0.2.0
```

For package-only recipe changes, keep the source version and bump the package
release field in the downstream package recipe, such as `pkgrel=2` in a
PKGBUILD.
