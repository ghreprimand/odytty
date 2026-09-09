# Named profiles

Named profiles capture a reusable launch context: which shell or command to run,
where to start, a bounded set of environment overrides, and appearance, cursor,
effects, and layout choices. A profile is opt-in. With no profile configured,
OdyTTY launches its built-in System Default, and the profile directory is never
scanned on that path.

This guide covers the on-disk format, the Profile Manager, how a launch resolves
a profile, every surface that can select one, defaults, optional host- and
directory-aware switching, and the security boundaries. For the internal design
contract see the [profiles foundation](v0.14.0-profiles-foundation.md).

Unreleased v0.15.0 development adds `quick_terminal_profile` for the dedicated
quick window. Profile resolution happens on summon. Quick access defaults off,
and global shortcut support and native-platform acceptance remain under
validation. See the [quick-terminal contract](v0.15.0-foundation.md#quick-terminal-role)
and [runtime settings](runtime-knobs.md) before using a development build.

## Contents

- [Where profiles live](#where-profiles-live)
- [Schema reference](#schema-reference)
- [Example profile](#example-profile)
- [Profile Manager](#profile-manager)
- [Default profiles](#default-profiles)
- [Selecting a profile at launch](#selecting-a-profile-at-launch)
- [Precedence](#precedence)
- [Optional host and directory switching](#optional-host-and-directory-switching)
- [Platform applicability and shell discovery](#platform-applicability-and-shell-discovery)
- [Import, export, and migration](#import-export-and-migration)
- [Security and recovery](#security-and-recovery)
- [Startup responsiveness](#startup-responsiveness)
- [Related settings](#related-settings)

## Where profiles live

Each profile is one hand-editable JSON document under the profiles directory:

```
<config-dir>/profiles/<name>.profile.json
```

`<config-dir>` is the same base OdyTTY uses for `odytty.conf` (see the
[install guide](install.md) for the per-platform location). The file name stem is
the profile name; the `.profile.json` suffix is required. A directory scan
returns at most 256 profiles, reads at most 1 MiB per file, and retains at most
100 parse warnings; entries beyond those bounds are skipped rather than allowed
to slow startup.

Files are written atomically with owner-private permissions (mode `0600` on
Unix, owner-restricted on Windows). Launch commands, directories, host aliases,
and environment values are personal metadata even when they are not credentials,
so profiles are never world-readable. A profile you export lands owner-private
too: the writer creates a private exclusive temporary file and renames it over
the destination, so neither the destination's prior mode nor your umask can
widen it. See [Import, export, and migration](#import-export-and-migration).

## Schema reference

The current document schema is `schema_version` 1. Within a supported version,
every schema-owned object preserves unknown keys through a save, so a profile
that carries fields a future build adds round-trips them intact. The version
gate is separate from that field preservation: `schema_version` must be a whole
number that does not exceed the version this build understands. A document
declaring a newer `schema_version` is rejected outright rather than partially
read, and a missing or zero version is rejected as malformed.

Top-level fields:

| Field | Type | Notes |
| --- | --- | --- |
| `schema_version` | integer | Required. Currently `1`. |
| `name` | string | Required. Matches the file stem. ASCII letters, digits, hyphen, underscore, or dot only; up to 64 characters. |
| `display_name` | string | Optional label shown in menus. Up to 512 characters. |
| `platforms` | array | Optional applicability list: `linux`, `macos`, `windows`. Absent means all. |
| `launch` | object | Shell, command, starting directory, and environment. |
| `appearance` | object | Theme, visual, font, title, and external-palette overrides. |
| `cursor` | object | Cursor style and blink overrides. |
| `effects` | object | Render-quality and effect overrides. |
| `layout` | object | Optional saved-layout reference. |
| `connection` | string | Optional connection alias used by SSH routing. |
| `switch` | object | Optional host- and directory-match rules for opt-in switching. |

`launch` fields:

| Field | Type | Notes |
| --- | --- | --- |
| `shell` | string | Interactive shell to run instead of the discovered default. |
| `command` | object | `program` (string) plus `args` (array, up to 32). Runs instead of a shell. |
| `working_directory` | string | Starting directory. |
| `env` | object | Bounded environment overrides. Up to 64 entries; values up to 1024 characters. |

`appearance` fields: `theme`, `visual`, `font`, `font_family`, `font_weight`
(strings), `font_size_px` (number), `title` (string), `follow_external_palette`
(boolean), `external_palette_provider` and `external_palette_path` (strings).
See [external palette following](v0.14.0-external-palette.md) and
[themes](themes.md).

`cursor` fields: `style`, `blink` (strings, drawn from the supported cursor
vocabulary). `effects` fields: `render_quality` (string), `bloom`, `crt`,
`retro` (booleans). `layout.saved_layout` (string) names a saved layout to open.
`switch.match_hosts` and `switch.match_directories` are string arrays, each up to
16 entries.

Every string field is bounded to 512 characters unless a different cap is noted
above. Environment values are the deliberate exception at 1024 characters, and
the profile `name` caps at 64. Any override left unset falls through to the
global configuration; a profile that customizes only environment or only
appearance is the common case.

## Example profile

`<config-dir>/profiles/dev.profile.json`:

```json
{
  "schema_version": 1,
  "name": "dev",
  "display_name": "Development",
  "platforms": [
    "linux",
    "macos"
  ],
  "launch": {
    "shell": "/usr/bin/zsh",
    "working_directory": "/home/user/src/project",
    "env": {
      "EDITOR": "nvim",
      "RUST_BACKTRACE": "1"
    }
  },
  "appearance": {
    "theme": "odyssey-nightshade",
    "title": "dev"
  },
  "cursor": {
    "style": "beam"
  }
}
```

A command profile replaces the shell instead:

```json
{
  "schema_version": 1,
  "name": "logs",
  "launch": {
    "command": {
      "program": "journalctl",
      "args": ["-f", "-u", "odytty"]
    }
  }
}
```

## Profile Manager

Open **Settings -> Profiles -> Open Profile Manager**. The manager is
presentation-only and loads the local catalog when it opens; it never runs on the
ordinary launch path. It offers create, edit, duplicate, rename, validate,
import, export, and delete.

- Every schema option is editable: shell, command and arguments, bounded
  environment overrides, appearance, cursor, effects, saved layout, connection
  reference, platform applicability, and switching rules.
- Command arguments, environment entries, host rules, and directory rules have
  explicit add, edit, and remove rows. The documented limits are enforced with
  an inline message rather than a silent truncation.
- Boolean overrides cycle inherit / on / off; enumerations cycle only supported
  values; platforms select inherit, Linux, macOS, Windows, or all without
  executing any platform command.
- The theme row cycles the built-in roster plus your theme files with Left and
  Right (typing a name still works). An unknown theme name is rejected at save,
  not silently ignored at launch.
- A starting directory you set or change is checked for existence at save. A
  value loaded unchanged from an existing profile round-trips even if it is
  momentarily missing.
- An environment row with an empty key or an empty value blocks save with an
  inline error rather than being dropped.
- Press `/` to filter the catalog. While filtering, every printable character
  reaches the query, so single-key hotkeys never steal a character of a name.
- The form and catalog scroll under the mouse wheel without moving focus and show
  a scroll indicator when more rows sit below the view. Shortcut legends stay
  visible at supported narrow widths.
- Deleting is destructive and requires confirmation.

Deleting or renaming the global default clears or rewrites the saved
`default_launch_profile`, and any workspace-scoped override naming that profile is
cleared or rewritten to match. Unknown future keys survive edit and save through
the schema round-trip.

## Default profiles

There are two independent defaults:

- **Global default.** Set it with **Set as Default** in the Profile Manager. It
  is stored as `default_launch_profile` in `odytty.conf`. New windows and unbound
  workspaces use it.
- **Workspace default override.** A workspace can bind a `launch_profile`. New
  tabs in that workspace use the workspace default when configured, otherwise the
  global default.

Two situations are handled differently:

- **Renamed or deleted inside the Profile Manager.** The manager rewrites or
  clears the saved `default_launch_profile`, and any workspace-scoped override
  naming that profile is rewritten or cleared to match, so the binding stays
  consistent with the catalog.
- **Missing, malformed, renamed, or deleted outside the manager.** When a saved
  default cannot be resolved at launch because its file was removed, renamed, or
  corrupted on disk, the launch falls back to the built-in System Default with a
  bounded one-line notice and leaves the saved default string untouched for you
  to repair.

CLI overrides, restored state, connection bindings, and automatic switching never
mutate either saved default.

The workspace `launch_profile` binding is distinct from the older workspace
`default_profile` field, which remains a connection-host alias for SSH New Tab
routing and is not treated as a named profile.

## Selecting a profile at launch

Plain **New Tab** and **New Workspace** (the `+` affordance and their keyboard
shortcuts) stay immediate and use the effective default profile. Every other
surface can select a specific profile:

| Surface | How |
| --- | --- |
| Profile chooser | The `▾` chevron beside the tab-strip and workspace-rail `+` opens the lazy-loaded picker. |
| Command palette | `Ctrl+Shift+P`, then a **New Tab: Profile ...** or **Bind Workspace to Profile ...** row. |
| Context menu | **New Tab with Profile...** and **New Workspace with Profile...** beside the plain rows. |
| Command line | `odytty --profile NAME` (also `--profile=NAME`). Supply a valid, nonempty profile name. |
| Connection manager | `Ctrl+Shift+S` selects a connection profile for SSH routing. |
| Saved layouts and restoration | A restored `profile_name` hint or a layout that references a profile re-applies it. |

The chooser is searchable and reachable from keyboard and context-menu surfaces.
Opening it loads the local catalog only at that moment. New Workspace with
Profile creates the workspace, binds its `launch_profile`, and spawns the first
tab atomically.

## Precedence

Effective launch resolution follows this order, later layers winning:

1. built-in defaults
2. global config file (`odytty.conf`)
3. named profile overrides (when a profile name resolves)
4. workspace named-profile binding (`launch_profile`)
5. startup environment variables
6. restored hints
7. explicit CLI overrides
8. live UI edits

For `working_directory` and `title`, a live UI edit wins over CLI, restored, and
profile values. The other launch fields (`shell`, `command`, `connection`,
`layout`, `env`) currently merge only the CLI and profile layers, with CLI
winning. The System Default path resolves no profile and does not scan the
profile directory.

## Optional host and directory switching

Automatic switching is off by default. Enable it with `profile_auto_switch = on`
in `odytty.conf`, or the `ODYTTY_PROFILE_AUTO_SWITCH` environment variable. When
on, OdyTTY evaluates a profile's `switch.match_hosts` and
`switch.match_directories` rules as the focused pane's working directory changes.

- Host patterns support exact names, `*`, and `*suffix` suffix wildcards (for
  example `*.example`).
- When the active profile still matches the current context, switching holds
  steady rather than alternating across repeated directory events.
- A match applies its appearance through the existing session seam and shows a
  short on-screen disclosure.
- Remote panes match host rules only against the trusted saved host identity.
  Terminal output can never select, create, or rewrite a profile.

## Platform applicability and shell discovery

A profile's optional `platforms` list marks where it applies (`linux`, `macos`,
`windows`). Built-in shell discovery is cached and runs only when a UI surface
asks for suggestions, never on the default launch path:

- **Windows** lists PowerShell, cmd, and installed WSL distributions read-only.
  Discovery never changes the OS default shell and never executes its output.
- **macOS and Linux** read `/etc/shells` and common login shells; discovery is
  explicit and separately tested.

Environment values pass through fork/exec on Unix and fold into the ConPTY
environment block on Windows. Environment keys and values are validated against
the platform's rules at the schema boundary before launch, so a profile cannot
carry a name the target platform's process environment cannot represent.

## Import, export, and migration

Import and export use the native file dialog. Import validates the document with
the same parser used for a normal load, so it rejects secrets and over-limit
fields before writing anything. Export is not a raw byte copy: it re-serializes
the profile through that same validating parser and writes the result to the
destination you choose, so an export can never emit secrets or over-limit fields
even if the on-disk file was edited by hand. The write goes through a private
exclusive temporary file that is renamed over the destination, so the exported
file is owner-private (`0600` on Unix, owner-restricted on Windows) and a failed
export leaves any existing destination file byte- and mode-identical.

Existing configuration migrates without data loss:

- Legacy connection-host appearance fields map into a named profile without
  moving transport fields out of `hosts.conf`.
- The workspace `default_profile` host-alias string is trimmed without being
  reinterpreted as a named profile.
- Older snapshots and config files are preserved.

## Security and recovery

- **No secrets.** Secret-shaped environment keys (`password`, `secret`, `token`,
  `credential`, `apikey`, `privatekey`, `identityfile`, and names containing
  them) are rejected across the whole document at parse time, and private-key
  material in any string value is rejected. Writes re-serialize through that same
  parser, so a programmatically built profile cannot persist what a file parse
  would reject.
- **Environment validation.** An empty key, a key containing `=`, a NUL in any
  key or value, and platform-invalid names are rejected before launch. Valid
  names, including names with spaces, round-trip literally.
- **Atomic writes.** Every save is atomic with owner-private permissions.
- **Malformed recovery.** A malformed profile file does not take down the
  manager: the parse failure is reported and other profiles continue to load.
  Import surfaces the same validation errors instead of writing a bad file.

## Startup responsiveness

Profiles never delay the first usable default terminal:

- The System Default launch reads no profile and does not scan the profile
  directory, run shell discovery, enumerate WSL distributions, or make remote
  checks.
- Profile enumeration, previews, and validation happen only when a UI surface
  such as the Profile Manager or the chooser is opened, or when a specific
  profile is actually selected.
- A configured global or workspace default triggers a single bounded local
  catalog read, never discovery or a remote check.

## Related settings

- `odytty.conf`: `default_launch_profile`, `profile_auto_switch`. See the
  [runtime reference](runtime-knobs.md).
- [Settings guide](settings-guide.md) for shipped defaults and opt-ins.
- [Keybindings](keybindings.md) for the command palette and connection manager
  chords.
- [Themes](themes.md) and [external palette following](v0.14.0-external-palette.md)
  for appearance overrides.
