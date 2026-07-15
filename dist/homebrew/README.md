# Publishing The Homebrew Tap

The live [`ghreprimand/homebrew-odytty` tap][brew] is OdyTTY's Homebrew
channel. The recipes in this directory are upstream templates; every tagged
release stamps and pushes them to the separate tap repository.

## Install From The Live Tap

The cask is the recommended Apple Silicon path:

```sh
brew tap ghreprimand/odytty
brew install --cask odytty
```

Recent Homebrew versions can require explicit trust before loading a cask from
a third-party tap. If Homebrew reports `Refusing to load cask ... from untrusted
tap`, trust this tap once and retry:

```sh
brew trust ghreprimand/odytty
brew install --cask odytty
```

The channel provides two recipes:

| Recipe | Intended use | Installed result |
| --- | --- | --- |
| `Casks/odytty.rb` | Recommended on Apple Silicon | Prebuilt, ad-hoc-signed `OdyTTY.app` |
| `Formula/odytty.rb` | Source fallback, including Intel Macs | Locally compiled `odytty` binary |

The app is ad-hoc signed but not notarized. macOS therefore quarantines the
download, and Gatekeeper would normally block its first launch. The cask's
postflight removes `com.apple.quarantine` from the installed app and discloses
that action in its caveats.

Users who do not want that quarantine change can install the source formula:

```sh
brew install odytty
```

The formula compiles locally and is not quarantined. It installs the command-line
binary rather than the `.app` bundle.

## Automatic Publishing

After a `vX.Y.Z` tag publishes the GitHub Release, the `homebrew` release job:

1. downloads the published `SHA256SUMS`;
2. reads the macOS app-zip and source-archive checksums;
3. stamps the cask version and checksum;
4. stamps the formula URL and checksum; and
5. pushes both recipes to the live tap.

The job skips an already-published version and retries push races. If the tap
credential is unavailable, it still downloads the checksum file and rewrites
both recipes as a validation gate, then exits cleanly without publishing.
That validation-only path keeps forks and replacement repositories usable.

## Verify A Tagged Release

Confirm the tap exposes the new version and that both installation paths parse:

```sh
brew update
brew info --cask ghreprimand/odytty/odytty
brew info ghreprimand/odytty/odytty
```

The live cask checksum must match the version-pinned
`odytty-<version>-macos-arm64.zip` entry in `SHA256SUMS`. The formula checksum
must match `odytty-<version>.tar.gz`.

## Historical Seed Values

The checked-in recipes retain historical `v0.8.1` seed values so the release
job always has complete Ruby files to rewrite. The formula carries the real
`v0.8.1` source-archive checksum.

The cask carries an all-zero checksum because `v0.8.1` predates the macOS app
artifact. That value is a template seed, not the live tap checksum and not a
valid release checksum. Each tagged release replaces it from the published
`SHA256SUMS` before any tap push.

## Platform Scope

The prebuilt cask targets Apple Silicon and requires macOS Big Sur or newer.
Intel Macs use the source formula until a universal binary is justified.

A Developer ID-signed and notarized `.dmg` remains deferred until the project
adopts the Apple Developer Program. Ad-hoc signing plus the disclosed
quarantine removal keeps the current path account-free.

[brew]: https://github.com/ghreprimand/homebrew-odytty
