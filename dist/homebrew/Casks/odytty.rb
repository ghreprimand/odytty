# Canonical source for the `odytty` Homebrew cask.
#
# The tap (`ghreprimand/homebrew-odytty`) holds its own git repository; this
# copy is the upstream template. On each release tag the `homebrew` job in
# .github/workflows/release.yml stamps `version` and `sha256` from the published
# release, then pushes the updated cask (and the source-build formula) to the
# tap — the same pattern the `scoop` and `aur` jobs use for their channels.
#
# The cask installs the prebuilt, ad-hoc-signed `OdyTTY.app` (Apple Silicon /
# arm64) that the release workflow's macOS leg produces. The app is ad-hoc
# signed but not notarized, so macOS quarantines the download and Gatekeeper
# would block the first launch; the postflight below clears the quarantine flag
# on the installed app so it launches cleanly with no manual step. Notarization
# (Apple Developer Program) would remove the need for that flag-clear. Intel
# Macs use the source-build formula instead.
#
# The `sha256` below is a placeholder: no macOS artifact existed at v0.8.1 (the
# macOS release leg landed afterward), so the auto-bump fills the real checksum
# on the first release that publishes `odytty-<version>-macos-arm64.zip`.
cask "odytty" do
  version "0.8.1"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"

  url "https://github.com/ghreprimand/odytty/releases/download/v#{version}/odytty-#{version}-macos-arm64.zip"
  name "OdyTTY"
  desc "Reliable, GPU-rendered terminal emulator with an OdysseyOS visual identity"
  homepage "https://github.com/ghreprimand/odytty"

  depends_on arch: :arm64
  depends_on macos: :big_sur

  app "OdyTTY.app"

  # OdyTTY.app is ad-hoc signed but not notarized, so macOS quarantines the
  # download and Gatekeeper blocks the first launch. Clear the quarantine flag
  # on the installed app so it launches without a warning. Tolerate a non-zero
  # exit (for example when the attribute is already absent) so a reinstall never
  # fails on this step.
  postflight do
    system_command "/usr/bin/xattr",
                   args: ["-r", "-d", "com.apple.quarantine", "#{appdir}/OdyTTY.app"],
                   must_succeed: false
  end
end
