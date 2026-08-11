# OdyTTY

[Website](https://odytty.unfinished-works.com) |
[Latest release](https://github.com/ghreprimand/odytty/releases/latest) |
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

The installer detects apt or dnf and installs the matching checksummed package;
other x86_64 systems receive the portable binary tarball:

```sh
curl -fsSL https://raw.githubusercontent.com/ghreprimand/odytty/master/dist/install.sh | bash
```

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
  feedback.
- **Workspaces and remote work:** tabs, resizable panes, named workspaces,
  layouts, restore, Unix managed and detached sessions, an SSH connection
  manager, connection reuse, and optional `tmux` persistence.
- **Configuration without ceremony:** a live settings panel, command palette,
  font and theme pickers, 142 built-in themes, user themes, a theme builder,
  backgrounds, transparency, bloom, CRT, and retro effects. Config-file editing
  remains available with hot reload.
- **Accessibility and privacy:** contrast controls, color-vision modes,
  dimming, motion controls, and a configurable bell. OdyTTY has no telemetry,
  analytics, crash reporting, account, cloud sync, or update ping; network
  actions are explicit and user-initiated.

Read the [feature reference](docs/features.md) for supported protocols,
workflows, settings, and platform-specific behavior.

## Status And Scope

OdyTTY is a broad pre-1.0 terminal. Version 0.10.0 is published. Bounded
post-release checks of the shipped Linux, macOS Apple Silicon, and Windows
packages completed without a reported blocker. A matched visual pass against
comparable terminal emulators found no release-blocking difference in the
tested text, Unicode, box-drawing, or interaction surfaces. These checks
complement blocking CI; they do not cover every GPU, compositor, IME, font, or
hardware configuration.

Linux is the primary target. macOS and Windows are supported, shipped, and
blocking CI targets. Known gaps include Windows detached and resumable session
hosting, profiles, Kitty animation and Unicode placeholders, iTerm2 graphics,
COLR/CPAL color fonts, and shaping beyond the default ASCII contextual path.
See [current work](TODO.md) and the [full roadmap](docs/full-build-roadmap.md).

The terminal core and visual experience layer are deliberately separate.
See the [ownership boundary](SPEC.md#ownership-boundary),
[module map](CONTRIBUTING.md#module-map), and
[visual pipeline](docs/visual-architecture.md) for the architecture.

## Build And Test

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

## Documentation

- [Install and update guide](docs/install.md)
- [Feature reference](docs/features.md)
- [Settings guide](docs/settings-guide.md) and
  [runtime reference](docs/runtime-knobs.md)
- [Keybindings](docs/keybindings.md)
- [Accessibility](docs/accessibility.md)
- [Diagnostics](docs/diagnostics.md)
- [Architecture specification](SPEC.md)
- [Complete documentation index](docs/README.md)

## Contributing, Security, And License

Read [CONTRIBUTING.md](CONTRIBUTING.md) before submitting changes. It includes
the test requirements, Developer Certificate of Origin, and public-repository
safety rules. Report vulnerabilities through the private process in
[SECURITY.md](SECURITY.md#reporting-a-vulnerability).

OdyTTY is licensed under **GPL-3.0-only**. You may use, study, share, and modify
the source under that license; distributed modifications must use the same
license. See [LICENSE](LICENSE).

Copyright (C) 2026 Unfinished Works and the OdyTTY contributors.

The OdyTTY name and branding are separate from the source license. Forks and
modified builds should use their own name and must not imply endorsement by
Unfinished Works. See [NOTICE](NOTICE).
