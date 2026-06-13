# OdyTTY theme files

OdyTTY themes are full appearance profiles: the default foreground/background,
the window clear color, the complete 16-color ANSI palette, and a set of
semantic role colors (cursor, selection, search highlight, plus reserved
border/inactive). A theme is selected with the `theme` setting
(`ODYTTY_THEME` / `theme =` in `odytty.conf`).

There are two kinds of theme:

- **Built-in themes**, selected by name — `plain` (the default —
  pixel-identical to the pre-theme appearance) plus a curated library of
  Odyssey-identity and community themes (see [Built-in theme
  library](#built-in-theme-library)).
- **User theme files**, written in the dependency-free theme file format
  described below and dropped into your theme directory (or referenced by path).

## Selecting a theme

```conf
# odytty.conf
theme = odyssey            # a built-in name
# theme = solarized        # a user theme: <config>/odytty/themes/solarized.theme
# theme = /path/to/my.theme  # a user theme by absolute/relative path
```

Resolution order for the `theme` value:

1. If it matches a built-in name (any theme in the [library](#built-in-theme-library)),
   the built-in is used.
2. Otherwise, if it looks like a path (contains `/` or ends in `.theme`), that
   file is read directly.
3. Otherwise it is looked up in the user theme directory as
   `<name>.theme`, then `<name>`.

If the value resolves to nothing, or the file cannot be read, OdyTTY falls back
to the `plain` theme and logs a warning. **A bad or missing theme value never
prevents startup.**

### Theme directory

| Base | Theme directory |
| --- | --- |
| `$XDG_CONFIG_HOME` set | `$XDG_CONFIG_HOME/odytty/themes/` |
| otherwise | `$HOME/.config/odytty/themes/` |

Theme files conventionally use the `.theme` extension.

### Live reload

Editing `odytty.conf` to point `theme` at a different built-in or user file
takes effect on the next reload poll, the same way every other setting reloads
— no restart needed. (Editing the *contents* of an already-selected theme file
is picked up the next time the config file itself changes; touch
`odytty.conf` to force a re-read.)

## Built-in theme library

OdyTTY ships a curated library of built-in themes, selectable by name with no
file needed. Every built-in is authored in the theme file format and loaded
through the same parser as a user theme — there is no privileged construction
path — so the file format is exercised by the library on every startup.

| Name | Appearance | Family |
| --- | --- | --- |
| `plain` | dark | OdyTTY default (pixel-identical to the pre-theme look) |
| `odyssey` | dark | Odyssey identity |
| `odyssey-noir` | dark | Odyssey identity (deep, low-key) |
| `odyssey-light` | light | Odyssey identity (light) |
| `odyssey-aurora` | dark | Odyssey identity (high-contrast) |
| `solarized-dark` | dark | Community |
| `solarized-light` | light | Community |
| `gruvbox-dark` | dark | Community |
| `nord` | dark | Community |
| `dracula` | dark | Community |
| `tokyo-night` | dark | Community |
| `catppuccin-mocha` | dark | Community |
| `catppuccin-latte` | light | Community |
| `one-dark` | dark | Community |
| `monokai` | dark | Community |

### Readability validation

Every built-in's default foreground/background pair is checked against a
minimum WCAG perceptual contrast ratio at build/test time. OdyTTY uses a floor
of **4.0** — just under the WCAG AA 4.5 threshold so that faithful community
palettes (Solarized in particular sits right at the boundary: ~4.1 light,
~4.75 dark) keep their authentic values rather than being silently retuned.

This is a *library-authoring* floor, not a render-time guarantee. Per-user
render-time contrast enforcement — a configurable minimum that lifts low-contrast
text from *any* app or theme — is the job of the upcoming minimum-contrast
feature (RV1), which builds on the same contrast helper
(`theme::contrast_ratio`) used to validate the library here.

## File format

The theme file format is line-oriented `key = value`, exactly like
`odytty.conf`, so there is only one syntax to learn:

- `#` at the **start of a line** begins a full-line comment. (Because colors
  begin with `#`, a `#` is only treated as a comment at the start of a line, or
  as an inline trailing comment when preceded by whitespace — `color0 = #112233
  # normal black` works.)
- Blank lines are ignored.
- Keys are case- and punctuation-insensitive: `color0`, `Color_0`, and
  `COLOR 0` are the same key.
- **Unknown keys are ignored with a warning** — a theme written for a newer
  OdyTTY still loads on an older build.
- A malformed value (bad hex, bad number) warns and leaves that one field at
  its default; one bad line never discards the whole theme.
- **Missing keys keep the `plain` baseline** for that slot, so partial themes
  (for example, one that only overrides the background and a few palette
  entries) are valid.

Colors are written as `#RRGGBB` or `#RGB` (the leading `#` is optional;
`#1a2b3c`, `1a2b3c`, and `#abc` are all accepted).

### Keys

| Key | Meaning |
| --- | --- |
| `name` | Display name. Defaults to `custom`. |
| `appearance` | `dark` or `light`. Metadata for future light/dark-aware features. |
| `foreground` (`fg`) | Default text color for `Color::Default` cells. |
| `background` (`bg`) | Default cell background. |
| `clear` | Window clear color; defaults to `background` when omitted. |
| `cursor` | Cursor color (semantic role). |
| `selection` | Selection background (semantic role). |
| `search` | Search-highlight background (semantic role). |
| `border` | Border/frame color (semantic role; reserved). |
| `inactive` | Inactive/dim color (semantic role; reserved). |
| `color0` … `color15` | The 16 ANSI colors (0–7 normal, 8–15 bright). Alias: `palette0` … `palette15`. |
| `font_family` | Optional font-family hint (forward-compat; not yet applied). |
| `font_size` | Optional font-size hint in px (forward-compat; not yet applied). |
| `visual` | Bundled visual-effect profile: `off`, `ambient`, or `scanlines` (forward-compat; not yet auto-applied). |

`appearance`, `font_family`, `font_size`, and `visual` are parsed, validated,
and round-tripped today but are **not yet applied at runtime** — they are part
of the theme schema so that future packets (a settings panel, a theme picker,
and the visual engine) can consume them without a format change.

## Example

This is the built-in `odyssey` theme written out in the file format. Copy it to
`~/.config/odytty/themes/my-odyssey.theme`, tweak the colors, and select it with
`theme = my-odyssey`:

```conf
# OdyTTY theme
name = odyssey
appearance = dark

foreground = #d6def4
background = #0c1224
clear = #070b18

cursor = #86c1ff
selection = #243352
search = #4a4018
border = #1b243e
inactive = #5a6480

color0 = #12182a
color1 = #e06b74
color2 = #98c379
color3 = #e5c07b
color4 = #61afef
color5 = #c68aee
color6 = #56b6c2
color7 = #c5cde0
color8 = #3a445e
color9 = #ff8b92
color10 = #b6e399
color11 = #ffd99a
color12 = #7fc1ff
color13 = #d8a6ff
color14 = #7ad4df
color15 = #f0f4ff
```

## Precedence with running applications

Application-driven color changes still win over the theme. A program that sets
palette entries via `OSC 4` (or the default fg/bg via `OSC 10/11`) overrides the
theme's colors for the lifetime of that session; the theme provides the
baseline those overrides start from. Selecting a new theme re-establishes the
baseline.
