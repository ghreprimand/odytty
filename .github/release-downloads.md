### Downloads

**Linux** ships four choices: the portable `odytty-x86_64.AppImage`,
a prebuilt binary tarball `odytty-linux-x86_64.tar.gz`, a native
`odytty-amd64.deb` (apt/dpkg), and a best-effort `odytty-x86_64.rpm`
(dnf/rpm). The version-pinned `odytty-@VERSION@-install.sh`
installer is checksum-covered by the signed manifest; download,
inspect, and run that asset after verifying the manifest signature.
**Windows** is `odytty-windows-x86_64.zip` (Scoop); **macOS** is
`odytty-macos-arm64.zip` (Homebrew cask).

`odytty-x86_64.AppImage`, `odytty-linux-x86_64.tar.gz`,
`odytty-amd64.deb`, `odytty-x86_64.rpm`,
`odytty-windows-x86_64.zip`, `odytty-macos-arm64.zip`, and
`odytty.tar.gz` are the **always-latest** names — use these for a
stable URL that resolves to the newest release. The
`…-@VERSION@-…` files are the **identical**
version-pinned copies (same bytes), for when you want to pin a
specific version. Every file is checksummed in `SHA256SUMS`; each
alias and its version-pinned twin therefore show matching hashes.
Verify `SHA256SUMS` against `SHA256SUMS.minisig` and the published
[OdyTTY release key](https://github.com/ghreprimand/odytty/blob/master/docs/keys/odytty-release.pub)
before trusting those hashes.

---
