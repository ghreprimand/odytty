# OdyTTY

Published release: **v0.14.0**.

Current development work is tracked in [TODO.md](TODO.md). Release evidence
and historical corrections are listed in the [release index](docs/releases/README.md).

[Website](https://odytty.unfinished-works.com) |
[Latest release](https://github.com/ghreprimand/odytty/releases/latest) |
[Release notes](docs/releases/README.md) |
[Install guide](docs/install.md) |
[Feature reference](docs/features.md) |
[Documentation](docs/README.md) |
[Issues](https://github.com/ghreprimand/odytty/issues)

![OdyTTY rendering a colorized git graph, project tree, and truecolor gradients under the default Odyssey theme with bloom](assets/demo.png)

**A from-scratch, GPU-rendered Rust terminal with an Odyssey visual identity.**

OdyTTY owns the terminal path from the PTY through escape parsing, terminal
state, text layout, and shaders. It combines that foundation with readable GPU
text, tabs and panes, inline media, in-app configuration, accessibility
controls, and optional visual effects. It is Linux-first, with packaged macOS
Apple Silicon and Windows releases, and runs independently of OdysseyOS.

## Install

Choose the recommended release for your platform. The
[full install guide](docs/install.md) covers alternate packages, checksums,
source builds, signing prompts, desktop integration, default-terminal setup,
and troubleshooting.

### Linux

The version-pinned installer detects apt or dnf and installs the matching
signature-verified package; other x86_64 systems receive the portable binary
tarball. Paste this block to install or update to the latest release. It
automatically resolves the version and verifies the installer before running it:

```sh
bash <<'ODYTTY_UPDATE'
set -euo pipefail
command -v minisign >/dev/null || { echo 'Install minisign first, then rerun this block.' >&2; exit 1; }
workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT
cd "$workdir"
release=$(curl -fsSL -o /dev/null -w '%{url_effective}' https://github.com/ghreprimand/odytty/releases/latest)
version=${release##*/v}
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo 'Could not resolve the latest release.' >&2; exit 1; }
base="https://github.com/ghreprimand/odytty/releases/download/v${version}"
curl -fLO "${base}/odytty-${version}-install.sh"
curl -fLO "${base}/SHA256SUMS"
curl -fLO "${base}/SHA256SUMS.minisig"
minisign -Vm SHA256SUMS -x SHA256SUMS.minisig -P 'RWQcOPw3PisdAGt2Q2IF7W6P1sgyPs2b9rQvFJohmLC8/w+qJt+aXEev'
awk -v file="odytty-${version}-install.sh" '$2 == file' SHA256SUMS | sha256sum -c -
bash "odytty-${version}-install.sh"
ODYTTY_UPDATE
```

This needs `minisign` and `sha256sum`. A mutable `curl | bash` convenience
command is intentionally not the trusted installation path; it executes an
unreviewed network response before it can verify anything.

Arch users can install `odytty` from the AUR with `paru -S odytty` or
`yay -S odytty`. Direct `.deb`, `.rpm`, AppImage, binary-tarball, and source
paths are documented in the [Linux install guide](docs/install.md#linux).
OdyTTY prefers Vulkan, also supports accelerated OpenGL/GLES, and treats
software rendering as a slow last resort. Wayland is the primary display
target; X11 is supported through the current windowing and GPU stack.

### macOS

The Homebrew cask installs the prebuilt Apple Silicon app:

```sh
brew tap ghreprimand/odytty
brew install --cask odytty
```

The app is ad-hoc signed rather than notarized; the cask handles its disclosed
quarantine-clearing step. Intel Macs currently use the
[source build](docs/install.md#build-from-source).

### Windows

With [Scoop](https://scoop.sh) installed:

```powershell
scoop bucket add odytty https://github.com/ghreprimand/odytty
scoop install odytty
```

The release is unsigned, so Windows may show a SmartScreen prompt. Scoop
verifies the checksum, adds `odytty` to `PATH`, and creates a Start-menu entry.
See the [Windows install guide](docs/install.md#windows) for the portable zip,
first-launch steps, and current platform scope.

## Update

Use the same channel that installed OdyTTY:

| Installed with | Update |
| --- | --- |
| Linux installer | Re-run the installer command above. |
| Direct `.deb` or `.rpm` | Re-run the installer, or download and install the latest package. OdyTTY does not publish an apt or dnf repository. |
| AUR | Run `paru -Syu` or `yay -Syu`; for a manual checkout, run `git pull --ff-only` and `makepkg -si`. |
| AppImage or tarball | Replace it with the always-latest artifact, verify `SHA256SUMS`, and reuse the previous install location. |
| Homebrew | Run `brew update && brew upgrade --cask odytty`. |
| Scoop | Run `scoop update && scoop update odytty`. |
| Source | Update the source tree, rebuild with `cargo build --release --locked`, and reinstall to the same prefix. |

The [update guide](docs/install.md#updating) provides exact commands for every
release format.

## Run And Configure

Open the default shell or launch a command directly:

```sh
odytty
odytty -e btop
```

Most customization is available inside the app:

| Action | Shortcut |
| --- | --- |
| Settings | `Ctrl+Shift+,` |
| Command palette | `Ctrl+Shift+P` |
| Theme picker | `Ctrl+Shift+H` |

Hand-editing is optional. When used, `odytty.conf` lives under
`$XDG_CONFIG_HOME/odytty/` or `~/.config/odytty/` on Unix and
`%APPDATA%\odytty\` on Windows. See the [settings guide](docs/settings-guide.md),
[keybindings](docs/keybindings.md), and
[launch CLI reference](docs/runtime-knobs.md#launch-cli) for the complete
surface, including command hold, application identity, layouts, and detached
sessions.

## Highlights

- **Owned terminal foundation:** OdyTTY implements its PTY integration,
  DEC/xterm parser, bounded terminal model, input mapping, render geometry, and
  shaders. Unix systems use the Unix backend and Windows uses ConPTY.
- **GPU text and inline media:** bundled and system fonts, fallback chains,
  HiDPI rebuilds, color emoji where a supported color font is available, Kitty
  graphics, and Sixel share the `wgpu` renderer.
- **Daily terminal interaction:** Kitty keyboard support, broad mouse modes,
  IME, search, selection and copy mode, bracketed paste, hyperlinks, clickable
  paths, prompt navigation, keyboard hints, and transient resize and zoom
  feedback. With bracketed paste disabled, multiline or control-bearing source
  text is held behind a bounded escaped preview with original line/byte counts
  and explicit Paste, reversible Paste as One Line when available, or Cancel.
  Safe single-line and child-enabled bracketed paste retain their existing byte
  path; shells and editors such as Fish commonly enable that protected mode
  themselves. `warn_on_risky_paste = off` is an advanced global opt-out. See
  [Paste safety](docs/features.md#paste-safety) for the exact trigger matrix.
  Complete, current OSC 133 command ranges also expose output-only or
  prompt-inclusive select/copy, output-scoped search, failed-command
  navigation, and explicit bounded plain-text export. Missing, partial, or
  stale shell integration disables these actions instead of guessing.
  Bounded OSC 9/777 notifications, OSC 9;4 progress, one-shot command-finish
  notification, and pane activity/silence/bell/process/failure monitors use
  transient pane-owned state and generic OdyTTY wording. See
  [`docs/notifications.md`](docs/notifications.md).
- **Workspaces and remote work:** tabs, resizable panes, named workspaces,
  layouts, restore, Unix managed and detached sessions, an SSH connection
  manager, connection reuse, optional `tmux` persistence, and an on-demand
  searchable Session Navigator with bounded metadata, confirmed close actions,
  a process-lifetime fresh-shell reopen history, and an optional redacted preview.
- **Configuration without ceremony:** a live settings panel, command palette,
  font and theme pickers, 145 built-in themes in current unreleased source
  (144 in published v0.14.0), user themes, a theme builder
  with sliders and click-to-edit hex values (including capture of a pane's live
  colors into a new theme),
  backgrounds, transparency, bloom, CRT, and retro effects. Config-file editing
  remains available with hot reload.
- **Accessibility and privacy:** contrast controls, color-vision modes,
  dimming, motion controls, a configurable bell, and bounded notification
  presentation. OdyTTY has no telemetry,
  analytics, crash reporting, account, cloud sync, or update ping; network
  actions are explicit and user-initiated.

Read the [feature reference](docs/features.md) for supported protocols,
workflows, settings, and platform-specific behavior.

## Status And Scope

OdyTTY is a broad pre-1.0 terminal. Version 0.14.0 is published,
adding named launch profiles, external palette following, and a unified Session
Navigator, and closing an external security review. Named profiles have a
versioned no-secret on-disk schema, deterministic precedence, atomic storage
with malformed-file recovery, and a Profile Manager that exposes the complete
schema; plain New Tab and New Workspace stay one-click on the effective default
profile while an adjacent chooser and the context menus open a lazy searchable
picker, and profile discovery never delays the first local terminal. The
[named profiles guide](docs/profiles.md) covers the schema, Profile Manager,
defaults, precedence, launch surfaces, and switching. External
palette following is an optional opt-in that applies a complete local palette
file through the existing theme seam with fail-closed parsing and last-known-good
retention. The Session Navigator searches workspaces, tabs, panes, and
detachable sessions with a redacted opt-in preview. The release index above
records the published version.

The v0.14.0 work does not optimize rendering, terminal storage, GPU allocation,
or presentation timing, so it carries forward rather than relabels the v0.12.0
performance evidence.

That preregistered v0.12.0 W6 run records 89.0 MB current and 130.7 MB peak
memory on the benchmark environment, down 68.9 and 60.1 percent respectively
from the prior OdyTTY result and below Kitty and Ghostty in both memory
measures. Idle CPU remains in the same low band as Kitty and Alacritty.
Separately classified software-endpoint results, memory composition, and
scrollback scaling are published alongside W6 without pooling their evidence
classes; W7's four-hour memory-growth workload remains explicitly deferred.

The tagged v0.14.0 release passed blocking Linux, macOS, and Windows CI,
release publication, signed checksums for all 17 assets, seven byte-identical
alias pairs, platform provenance checks, and Scoop, Homebrew, and AUR
propagation. Profile and navigator hands-on acceptance and release-image testing
are complete. An isolated clean release build from the signed source archive
also passed on Linux with Rust 1.97.1; this was not a repeat MSRV verification.
These results do not imply exhaustive device or application coverage. Full
evidence and limitations are recorded in the [release
guide](docs/release.md). The full benchmark results remain in
[docs/benchmark-results.md](docs/benchmark-results.md); carried-forward results
do not cover every GPU, compositor, IME, font, or hardware configuration.

Linux is the primary target. macOS and Windows are supported, shipped, and
blocking CI targets. Known gaps include Windows detached and resumable session
hosting, full bidi and complex-script reordering, and SVG-in-OpenType color
glyphs.
The [v0.13.0 foundation contract](docs/v0.13.0-foundation.md) records the
security, architecture, platform, and measurement boundaries carried forward
from v0.13.0. Named launch profiles have a versioned on-disk foundation and a settings Profile
Manager for local create/edit/import/export/delete. The editor exposes the
complete profile schema, including bounded launch and switching lists, visual
settings, cursor/effect overrides, saved layout, and platform applicability
([schema, precedence, and migration](docs/v0.14.0-profiles-foundation.md));
launch routing, restoration, palette selection, and opt-in auto-switch are
wired. Plain `+` / New Tab / New Workspace stay one-click on the effective
default profile; an adjacent chevron and the context menus open a lazy
searchable profile chooser. Profile Manager sets an explicit global default
(`default_launch_profile`) that a workspace binding can override; without one,
startup and new tabs use the built-in System Default and never scan the
profile directory.
[External palette following](docs/v0.14.0-external-palette.md) is an optional
opt-in that applies a complete local palette file through the existing theme
seam without delaying ordinary startup. See
[current work](TODO.md) and the
[full roadmap](docs/full-build-roadmap.md) for later milestones.

Current v0.15.0 development adds the `odyssey-electric-blue` preset through the
existing cross-platform theme path. It does not change the default theme or any
effect setting. See the [theme reference](docs/themes.md#electric-blue).

The same development line now contains stable tab and workspace identities,
same-process window merge foundations, cross-platform quick-terminal shortcut
backends, and a bounded structural-control protocol with an explicit Unix CLI.
These surfaces remain unreleased. Ordinary startup opens no automation endpoint,
Windows named-pipe transport is pending, native file drop is not enabled, and
live-device and three-platform acceptance remain open. See the
[v0.15.0 contracts](docs/v0.15.0-foundation.md) for the exact boundaries.

The terminal core and visual experience layer are deliberately separate.
See the [ownership boundary](SPEC.md#ownership-boundary),
[module map](CONTRIBUTING.md#module-map), and
[visual pipeline](docs/visual-architecture.md) for the architecture.

## Build And Test

Release preparation requires matching versioned notes and includes their short
summary and canonical link above the download and verification instructions.

OdyTTY pins Rust 1.96 as its verified minimum supported version. The repository
toolchain file selects it automatically when Rust is managed by `rustup`.

```sh
cargo build --release --locked
cargo test
cargo fmt --check
```

The default test suite is bounded and deterministic. Blocking CI adds Clippy,
platform builds, and a production-file architecture guard; scheduled lanes run
deeper fuzzing, Miri, and sanitizers. See the
[contribution guide](CONTRIBUTING.md#test-battery) for the complete test battery,
platform gates, and pre-commit checks.

The project's maturity evidence is public and reproducible: the
[compatibility corpus](docs/compatibility/corpus.md) turns conformance,
real-application, differential, parser, and fuzz findings into permanent
regressions; the [pinned vttest runner](docs/compatibility/vttest.md) records
conformance results; the [fuzzing](fuzz/parser_graphics/README.md) and
[mutation-testing](docs/mutation-testing.md) campaigns exercise hostile and
fault-injected paths; and the
[published benchmarks](docs/benchmark-results.md) follow a preregistered
protocol. These are stronger claims than an unmeasured user-count proxy, while
still not replacing wider third-party soak exposure.

## Documentation

- [Install and update guide](docs/install.md)
- [Feature reference](docs/features.md)
- [Paste safety and risky-paste triggers](docs/features.md#paste-safety)
- [Named-profile foundation and precedence](docs/v0.14.0-profiles-foundation.md)
- [External security review closure ledger](docs/security-review-2026-09.md)
- [External palette following](docs/v0.14.0-external-palette.md)
- [Settings guide](docs/settings-guide.md) and
  [runtime reference](docs/runtime-knobs.md)
- [Keybindings](docs/keybindings.md)
- [Accessibility](docs/accessibility.md)
- [Diagnostics](docs/diagnostics.md)
- [Benchmarks: protocol, apparatus, and results](docs/benchmark-results.md)
- [Architecture specification](SPEC.md)
- [Complete documentation index](docs/README.md)

## Contributing, Security, And License

Contributions are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md) for what
lands easily, the test requirements, the Developer Certificate of Origin, and
the public-repository safety rules. Use the structured
[bug-report](https://github.com/ghreprimand/odytty/issues/new?template=bug_report.yml),
[change-proposal](https://github.com/ghreprimand/odytty/issues/new?template=change_proposal.yml),
or [question](https://github.com/ghreprimand/odytty/issues/new?template=question.yml)
form rather than guessing which route fits. Report vulnerabilities through the
private process in [SECURITY.md](SECURITY.md#reporting-a-vulnerability).

OdyTTY is licensed under **GPL-3.0-only**. You may use, study, share, and modify
the source under that license; distributed modifications must use the same
license. See [LICENSE](LICENSE).

Copyright (C) 2026 Unfinished Works and the OdyTTY contributors.

The OdyTTY name and branding are separate from the source license. Forks and
modified builds should use their own name and must not imply endorsement by
Unfinished Works. See [NOTICE](NOTICE).
