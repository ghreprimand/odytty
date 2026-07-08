# Homebrew tap (`ghreprimand/homebrew-odytty`)

This directory is the upstream source of truth for OdyTTY's [Homebrew][brew]
channel. The tap itself is a **separate** git repository
(`ghreprimand/homebrew-odytty`); the files here are copied there and pushed at
each release by the `homebrew` job in `.github/workflows/release.yml` — the same
pattern the `scoop` and `aur` jobs use for their channels.

The channel is **cask-primary with a source-formula fallback**:

- **`Casks/odytty.rb`** — the advertised path. Installs the prebuilt,
  ad-hoc-signed `OdyTTY.app` (Apple Silicon / arm64) that the release workflow's
  macOS leg produces. A cask install strips the download's
  `com.apple.quarantine` attribute, so the ad-hoc signature is enough to launch
  without a Gatekeeper warning — no Apple Developer account or notarization.

  ```sh
  brew tap ghreprimand/odytty
  brew install --cask odytty
  ```

- **`Formula/odytty.rb`** — the fallback. Builds the CLI binary from the
  published source tarball (`depends_on "rust" => :build`). A locally compiled
  binary is never quarantined, so it launches warning-free on any supported
  macOS, **including Intel Macs**, which the arm64-only cask does not target.

  ```sh
  brew install odytty
  ```

## Activation (operator-gated)

The recipes and the release auto-bump job are in-repo, but the channel is
**inert until the operator opts in** — exactly like the guarded AUR job. Two
one-time steps activate it:

1. **Create the tap repo.** It must be named `homebrew-odytty` (Homebrew
   requires the `homebrew-<name>` form; users then `brew tap ghreprimand/odytty`).
   Seed it with `Casks/odytty.rb` and `Formula/odytty.rb` from this directory.
2. **Add the push credential.** A write-scoped **deploy key** on
   `homebrew-odytty`, stored on this repo as the `HOMEBREW_TAP_DEPLOY_KEY`
   secret. Until it is set, the `homebrew` release job runs but **exits cleanly
   without publishing** (it still validates and rewrites the recipes locally as
   a gate). Adding the secret flips auto-publish on with no further change.

## How the auto-bump works

On a `vX.Y.Z` tag push, after the GitHub Release and its `SHA256SUMS` exist, the
`homebrew` job downloads `SHA256SUMS`, reads the `odytty-<version>-macos-arm64.zip`
checksum (for the cask) and the `odytty-<version>.tar.gz` checksum (for the
formula), rewrites `version`/`url`/`sha256` in both recipes, and pushes them to
the tap. It is idempotent (skips if the tap already carries the version) and
retries on push races, mirroring the Scoop and AUR jobs.

## Scope

- **arm64 cask only (v1).** Intel Macs use the source formula until a universal2
  (`lipo` arm64 + x86_64) cask is worth the extra CI build.
- **A signed/notarized `.dmg` is deferred** to if/when the Apple Developer
  Program is adopted. Ad-hoc signing plus the cask covers the account-free path.
- The `sha256` seeded in `Casks/odytty.rb` is a placeholder: no macOS artifact
  existed at v0.8.1, so the auto-bump fills the real checksum on the first
  release that publishes a macOS zip.

[brew]: https://brew.sh
