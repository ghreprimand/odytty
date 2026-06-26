# AUR package (`odytty`)

This directory is the upstream source of truth for the [AUR][aur] package
`odytty`. The AUR itself is a separate git repository; the files here
(`PKGBUILD`, `.SRCINFO`) are copied there and pushed at each release.

The package builds from the published GitHub release **source tarball** — the
same `git archive` artifact the Release workflow attaches — so an installed
`odytty` is versioned, owned by pacman, removable, and reproducible from a fixed
source. This is a best-effort artifact: it is built by users with `makepkg`, not
pre-built or smoke-tested by upstream CI.

## First-time setup (maintainer)

```sh
git clone ssh://aur@aur.archlinux.org/odytty.git aur-odytty
cd aur-odytty
cp /path/to/odytty/dist/aur/PKGBUILD .
```

## Publishing a release

1. Wait until the GitHub Release for the tag exists and its
   `odytty-<version>.tar.gz` asset is attached (the Release workflow uploads it).
2. Bump `pkgver` (and reset `pkgrel=1`) in `dist/aur/PKGBUILD`.
3. Fill in the real checksum from the published tarball:

   ```sh
   updpkgsums            # rewrites sha256sums=() from the downloaded source
   ```

4. Regenerate the metadata index:

   ```sh
   makepkg --printsrcinfo > .SRCINFO
   ```

5. Build-test in a clean chroot before publishing:

   ```sh
   makepkg -f            # or: extra-x86_64-build (devtools, clean chroot)
   namcap PKGBUILD *.pkg.tar.zst
   ```

6. Commit both files and push to the AUR remote:

   ```sh
   cp dist/aur/PKGBUILD dist/aur/.SRCINFO /path/to/aur-odytty/
   cd /path/to/aur-odytty
   git commit -am "odytty <version>-1"
   git push
   ```

> The `sha256sums=('SKIP')` checked in here is a placeholder. `updpkgsums`
> replaces it with the real release-tarball checksum; do not publish an AUR
> revision with `SKIP`.

## Notes

- `pkgrel` bumps when the PKGBUILD changes but the source version does not.
- Runtime deps (`fontconfig`, `freetype2`, `vulkan-icd-loader`, `libxkbcommon`,
  `hicolor-icon-theme`) cover the GPU/text/desktop stack OdyTTY links against;
  the Vulkan ICD itself (Mesa, proprietary drivers) is the user's GPU driver and
  is not pulled in here.

[aur]: https://aur.archlinux.org/packages/odytty
