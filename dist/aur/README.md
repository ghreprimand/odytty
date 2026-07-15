# Publishing The AUR Package

OdyTTY publishes the [`odytty` AUR package][aur] automatically after each
tagged GitHub release. The templates in this directory are the upstream source
of truth; the AUR remains a separate Git repository.

## Automatic Publishing

The release workflow calls `.github/workflows/aur-publish.yml` after the GitHub
Release exists. That workflow:

1. stamps the tag version into `PKGBUILD` and resets `pkgrel` to `1`;
2. replaces the checksum placeholder from the published source archive;
3. regenerates `.SRCINFO` with `makepkg --printsrcinfo`;
4. checks `PKGBUILD` with `namcap`; and
5. pushes both generated files to the AUR repository.

The job is idempotent. If the AUR already carries the release version, it exits
without creating another commit.

Publishing requires the repository's AUR credential. When that credential is
unavailable, the workflow still stamps and validates the package, then exits
cleanly without pushing. This validation-only path keeps forks and replacement
repositories usable.

The same workflow has a manual trigger for retrying a failed publication without
replaying every release producer. Supply the release version with or without a
leading `v`; leaving it blank reads the version from `Cargo.toml` on `master`.

## Verify A Tagged Release

After the release workflow finishes, confirm the AUR page shows the new
`pkgver` and `pkgrel=1`. A local source build provides an additional check:

```sh
git clone https://aur.archlinux.org/odytty.git
cd odytty
makepkg -si
```

The package builds from the version-pinned GitHub Release source archive. It is
owned and removable through pacman, but remains a best-effort source package:
users build it with `makepkg`, and upstream CI validates metadata rather than
building it on every supported Arch configuration.

## Publishing By Hand When CI Is Unavailable

Use this fallback only when the automatic workflow cannot run. Wait for the
GitHub Release and its `odytty-<version>.tar.gz` source asset before starting.

Create a temporary package workspace so release stamping does not alter the
upstream templates:

```sh
version=X.Y.Z
workdir="$(mktemp -d)"
cp dist/aur/PKGBUILD dist/aur/.SRCINFO "$workdir/"
cd "$workdir"

sed -i \
  -e "s/^pkgver=.*/pkgver=${version}/" \
  -e "s/^pkgrel=.*/pkgrel=1/" \
  PKGBUILD
updpkgsums
makepkg --printsrcinfo > .SRCINFO
makepkg -f
namcap PKGBUILD ./*.pkg.tar.zst
```

Copy the validated files into a fresh AUR checkout, review the staged diff, and
publish:

```sh
version=X.Y.Z
git clone ssh://aur@aur.archlinux.org/odytty.git aur-odytty
cp PKGBUILD .SRCINFO aur-odytty/
cd aur-odytty
git add PKGBUILD .SRCINFO
git diff --cached
git commit -m "odytty ${version}-1"
git push origin HEAD:master
```

The `sha256sums=('SKIP')` value checked into the upstream template is only a
placeholder. `updpkgsums` must replace it with the published archive checksum;
never publish an AUR revision that still contains `SKIP`.

Increment `pkgrel` instead of `pkgver` when only the packaging recipe changes.
Reset `pkgrel` to `1` for every new upstream version.

## Package Dependencies

The runtime dependencies cover the GPU, text, and desktop stack:
`fontconfig`, `freetype2`, `vulkan-icd-loader`, `libxkbcommon`, and
`hicolor-icon-theme`. The Vulkan ICD itself comes from the user's Mesa or vendor
graphics driver and is not installed by this package.

[aur]: https://aur.archlinux.org/packages/odytty
