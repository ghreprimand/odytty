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

Version 0.15.0 adds `quick_terminal_profile` for the dedicated quick window.
Profile resolution happens on summon; quick access defaults off. See the
[quick-terminal contract](v0.15.0-foundation.md#quick-terminal-role)
and [runtime settings](runtime-knobs.md).

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
examines at most 1,024 directory entries of any kind (other files, folders,
and malformed profiles all count), returns at most 256 profiles, reads at most
1 MiB per file, and retains at most 100 parse warnings. A document is strict JSON:
a raw control character inside a string, a number outside the JSON grammar or
beyond a 64-bit float, a repeated decoded key, and more than 4,096 members in
any object (including unknown objects) are refused with a
message naming the problem. A save is refused when the pretty-printed document
would exceed that 1 MiB read limit, so a saved profile always reloads. A file saved with a
leading UTF-8 byte order mark, as some Windows editors write, loads normally;
the same applies to `odytty.conf`, theme files, and external palette files. Profile files load in
name order. A profile is trusted like `odytty.conf`: its `theme`, `font`, and
`external_palette_path` values may name any file your account can read, so
install only profiles you would put in your own configuration. A scan that stops at either limit adds a warning, which the Profile
Manager shows. OdyTTY keeps the last parsed catalog and reuses it while every
profile file's name, size, and modification time are unchanged, together with
its inode, change time, and mode on Unix, or its creation time and file
attributes on Windows. On Unix any write, rename, removal, replacement, or
permission change reloads the catalog. On Windows a write, rename, removal,
size change, new creation time, or attribute change (read-only, hidden)
reloads it; an ACL-only change, or a same-size replacement that keeps the
modification and creation times (NTFS file-name tunneling can carry the
creation time over), is not seen until a later detected change or a restart.

Files are written atomically with owner-private permissions (mode `0600` on
Unix, owner-restricted on Windows). Launch commands, directories, host aliases,
and environment values are personal metadata even when they are not credentials,
so profiles are never world-readable. A profile you export lands owner-private
too: the writer creates a private exclusive temporary file and renames it over
the destination, so neither the destination's prior mode nor your umask can
widen it. See [Import, export, and migration](#import-export-and-migration).

## Schema reference

The current document schema is `schema_version` 1. Within a supported version,
schema-owned objects retain unknown keys for saving, including a `switch`
object that holds only unknown keys. An unknown number is kept as written when
its literal has more than 15 significant digits, which a 64-bit float cannot
always hold exactly, so a large integer or a long decimal in a field this build
does not own survives a save unchanged; shorter numbers are written in their
shortest form, using exponent notation when that is shorter, so `1E+2` is
saved as `100` and `1e300` stays `1e300`. A field this build owns is always
written from its typed value, never from the source literal.
The version gate is separate from field preservation: `schema_version` must be a
whole number that does not exceed the version this build understands. A document
declaring a newer `schema_version` is rejected outright rather than partially
read, and a missing or zero version is rejected as malformed.

Top-level fields:

| Field | Type | Notes |
| --- | --- | --- |
| `schema_version` | integer | Required. Currently `1`. |
| `name` | string | Required. Matches the file stem. ASCII letters, digits, hyphen, underscore, or dot only; up to 64 characters. Windows device names (`con`, `prn`, `aux`, `nul`, `com0`-`com9`, `lpt0`-`lpt9`, with or without an extension) are refused on every platform, and a save is refused when another profile file differs from it only by letter case. |
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
| `shell` | string | Interactive shell to run instead of the discovered default. Not empty, no NUL. |
| `command` | object | `program` (string, not empty) plus `args` (array, up to 32). Runs instead of a shell. A NUL in the program or an argument is refused. |
| `working_directory` | string | Starting directory. No NUL. |
| `env` | object | Bounded environment overrides. Up to 64 entries; values up to 1024 characters. |

`appearance` fields: `theme`, `visual`, `font`, `font_family`, `font_weight`
(strings), `font_size_px` (a number above zero that fits a 32-bit float; the size in use is still clamped to 6-72 pixels), `title` (string), `follow_external_palette`
(boolean), `external_palette_provider` and `external_palette_path` (strings).
See [external palette following](v0.14.0-external-palette.md) and
[themes](themes.md).

`cursor` fields: `style`, `blink` (strings, drawn from the supported cursor
vocabulary). `effects` fields: `render_quality` (string), `bloom`, `crt`,
`retro` (booleans). `layout.saved_layout` (string) names a saved layout to open.
`switch.match_hosts` and `switch.match_directories` are string arrays, each up to
16 entries; an explicit `null` counts as absent, as for every other optional
field. A rule that can never match (a host with a space or path separator, a
directory that is not absolute or contains `..`) still loads, and the catalog
reports it as a warning naming the entry.

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

The profile catalog keeps an explicit mouse-wheel scroll position through
redraws. Keyboard navigation and filtering resume following the selected row.
Catalog filters and form text fields ignore control characters. Labels preserve
exact fits and truncate to the overlay renderer's display-cell width.

Open **Settings -> Profiles -> Open Profile Manager**. The manager is
presentation-only and loads the local catalog when it opens; it never runs on the
ordinary launch path. It offers create, edit, duplicate, rename, validate,
import, export, and delete.

- Every schema option is editable: shell, command and arguments, bounded
  environment overrides, appearance, cursor, effects, saved layout, connection
  reference, platform applicability, and switching rules.
- Editing or choosing a shell replaces the command and its arguments. Clearing
  the command program selects the default shell; typing a command program
  replaces a shell suggestion. Unedited commands retain their arguments.
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
  momentarily missing. On Windows a network or device directory you type
  (`\\server\share`, `\\wsl.localhost\Distro\home`) is kept as typed and is
  not probed at save, so a missing share is reported when the tab starts.
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
cleared or rewritten to match in every open window. The saved default is updated
first: if that write fails, the old profile file and every binding are kept and
the manager names the failure, so no default or binding is left naming a profile
that no longer exists. If the default moves but the old file cannot be removed,
the manager reports the leftover file. Unknown future keys survive edit and save
through the schema round-trip.

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

A profile or command-line `working_directory` is explicit input. On Windows it
may name a network or device directory; a directory reported by a program
(OSC 7), restored from a saved layout or inherited from the focused pane is
never trusted that far and falls back to the home directory (see
[the threat model](threat-model.md)).

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
Shells report the working directory at every prompt; while the profile
directory is unchanged, each report costs one bounded directory listing and
reuses the parsed catalog.

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
  explicit and separately tested. The registry read stops at 64 KiB and 512
  lines and keeps at most 256 candidates before any file is probed. A
  `Default` entry appears only when `SHELL` is set and non-empty. A shell given
  as a bare name is looked up on `PATH` only, never in the working directory:
  empty and relative `PATH` entries (`.`, `bin`) are skipped, on Windows too.
  On Windows a built-in shell that is already listed as the default keeps its
  `Default` label.

Environment values pass through fork/exec on Unix and fold into the ConPTY
environment block on Windows. A profile value replaces any variable of the same
name that the terminal sets itself, such as `TERM`, so the shell receives one
entry holding the profile value. On Windows the name match ignores case by
Windows' own ordinal rule, which also folds non-ASCII letters, so `Ä_X` and
`ä_X` are one variable. Environment keys and values are validated against
the platform's rules at the schema boundary before launch, so a profile cannot
carry a name the target platform's process environment cannot represent.
With shell integration on, a profile `HOME` is also where zsh reads its startup
files when no `ZDOTDIR` is inherited.

## Import, export, and migration

Import and export use the native file dialog. Import validates the document with
the same parser used for a normal load, including field limits, prohibited
credential-shaped keys, and private-key markers. These checks do not detect
arbitrary secrets in command arguments or otherwise permitted environment
values. Export re-serializes and validates the profile before writing it to the
destination you choose; inspect its contents before sharing it. The write goes
through a private exclusive temporary file that is renamed over the destination, so the exported
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
  material in any string value is rejected. The error names the key and the
  word it matched; the match is by substring, so a key such as
  `TOKENIZERS_PARALLELISM` is refused too and has to be set outside the profile. Writes re-serialize through that same
  parser, so a programmatically built profile cannot persist what a file parse
  would reject.
- **Environment validation.** An empty key, a key containing `=`, a NUL in any
  key or value, and platform-invalid names are rejected before launch. Valid
  names, including names with spaces, round-trip literally.
- **Atomic writes.** Every save is atomic with owner-private permissions.
- **Malformed recovery.** A malformed profile file does not take down the
  manager: the parse failure is reported and other profiles continue to load.
  Import surfaces the same validation errors instead of writing a bad file.
  Quarantining a malformed file claims `<name>.profile.json.bad`; later copies
  use `<name>.profile.json.bad.1`, `<name>.profile.json.bad.2`, and so on (up to
  100 copies). A non-replacing hard link claims each destination before the
  source is removed, so a competing backup is never overwritten. Inspection
  or link errors preserve the source, including on filesystems without hard
  links. If source removal fails, both names remain and an error is reported.
  This applies on Linux, macOS, and Windows; symbolic links are not quarantined.
- **Collision admission.** A save requires a complete directory listing within
  the 1,024-entry bound, plus an exhaustion probe. Listing failures, entry
  errors, and an unseen tail refuse the save. The check does not serialize
  concurrent directory edits.

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
