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

### Theme families

**Odyssey identity** — `plain` plus sixteen `odyssey-*` variants are original
themes designed for OdysseyOS. `plain` reproduces the historical xterm default
palette byte-for-byte and is the fallback when no theme is configured. The
`odyssey-*` variants span a range of visual moods across dark and light
appearances: deep-space and interstellar cold (`odyssey`, `odyssey-deepspace`,
`odyssey-pulsar`), warm atmospheric (`odyssey-solar`, `odyssey-ember`,
`odyssey-abyss`), cool natural (`odyssey-glacier`, `odyssey-voyager`), cosmic
nebula (`odyssey-nebula`, `odyssey-aurora`), warm and cool text focus
(`odyssey-meridian`, `odyssey-graphite`), and three light companions
(`odyssey-light`, `odyssey-dawn-light`, `odyssey-sandstone-light`). These carry
the strongest OdysseyOS visual identity.

**Community** — twenty-eight themes ported from widely-used open-source
color-scheme palettes. The dark side covers the ten palettes that formed the
original community batch (Solarized, Gruvbox, Nord, Dracula, Tokyo Night,
Catppuccin Mocha, One Dark, Monokai) plus an extended set including Everforest,
Kanagawa, Rose Pine, Ayu Mirage, Night Owl, Palenight, GitHub-style dark,
Zenburn, Oceanic Next, and Iceberg. The light side covers ten counterparts:
Solarized Light, Catppuccin Latte, GitHub-style light, Gruvbox Light, One
Light, Ayu Light, Rose Pine Dawn, Tokyo Night Day, PaperColor, and Everforest
Light. Sources and licenses are listed in the
[attribution table](#attribution-and-licensing) below.

**Retro / phosphor** — eight themes inspired by historical display hardware:
three green-phosphor monochrome variants (`green-phosphor`, `ibm-5151`,
`vt220-green`), two amber monochrome variants (`amber-crt`, `hercules-amber`),
an Apple II-inspired green phosphor (`apple-ii-green`), the Commodore 64
blue-on-purple character screen (`commodore-64`), and a DOS/CGA sixteen-color
text palette (`dos-cga`) tuned to the canonical ANSI hue angles. These use
deliberately narrow ANSI palettes that approximate the look of the original
hardware while meeting the library's 4.0 contrast floor. Vendor and platform
names that appear in theme titles are trademarks of their respective owners;
OdyTTY has no affiliation with or endorsement from any of those vendors.

| Name | Appearance | Family |
| --- | --- | --- |
| `plain` | dark | OdyTTY default (pixel-identical to the pre-theme look) |
| `odyssey` | dark | Odyssey identity |
| `odyssey-noir` | dark | Odyssey identity (deep, low-key) |
| `odyssey-light` | light | Odyssey identity (light) |
| `odyssey-aurora` | dark | Odyssey identity (high-contrast) |
| `odyssey-deepspace` | dark | Odyssey identity (near-black interstellar void with cold starlight accents) |
| `odyssey-nebula` | dark | Odyssey identity (magenta and violet emission clouds with teal stellar accents) |
| `odyssey-solar` | dark | Odyssey identity (warm amber solar flare over a dark sunspot field) |
| `odyssey-abyss` | dark | Odyssey identity (deep teal hadal voyage with bioluminescent cyan) |
| `odyssey-ember` | dark | Odyssey identity (banked coals and forge orange against charcoal) |
| `odyssey-glacier` | dark | Odyssey identity (polar blue ice with cold high-legibility contrast) |
| `odyssey-meridian` | dark | Odyssey identity (warm parchment text on indigo twilight) |
| `odyssey-voyager` | dark | Odyssey identity (muted expedition greens with logbook warmth) |
| `odyssey-pulsar` | dark | Odyssey identity (neutron-star neon over near-black) |
| `odyssey-dawn-light` | light | Odyssey identity (violet-white morning companion to deep space) |
| `odyssey-sandstone-light` | light | Odyssey identity (sunlit dune and planetary-surface warmth) |
| `odyssey-graphite` | dark | Odyssey identity (minimal neutral graphite focus mode) |
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
| `everforest-dark` | dark | Community — forest-toned low-contrast dark palette |
| `kanagawa` | dark | Community — ink-and-wave dark palette |
| `rose-pine` | dark | Community — dusky rose and pine palette |
| `ayu-mirage` | dark | Community — muted blue-gray dark palette |
| `night-owl` | dark | Community — blue night palette |
| `palenight` | dark | Community — Material-lineage violet night palette |
| `github-dark` | dark | Community — GitHub-style dark palette, no affiliation |
| `zenburn` | dark | Community — low-glare classic dark palette |
| `oceanic-next` | dark | Community — deep ocean blue-gray palette |
| `iceberg-dark` | dark | Community — cool blue high-latitude dark palette |
| `github-light` | light | Community — GitHub-style light palette, no affiliation |
| `gruvbox-light` | light | Community — warm retro light palette |
| `one-light` | light | Community — Atom-style light palette |
| `ayu-light` | light | Community — bright neutral light palette |
| `rose-pine-dawn` | light | Community — soft dawn companion to Rose Pine |
| `tokyo-night-day` | light | Community — Tokyo Night light palette |
| `papercolor-light` | light | Community — paper-inspired terminal palette |
| `everforest-light` | light | Community — warm forest light palette |
| `green-phosphor` | dark | Retro — P1-CRT-inspired green monochrome |
| `amber-crt` | dark | Retro — P3-amber-inspired monochrome |
| `ibm-5151` | dark | Retro — IBM 5151-inspired green monochrome, no affiliation |
| `dos-cga` | dark | Retro — DOS/CGA-inspired ANSI text palette |
| `apple-ii-green` | dark | Retro — Apple II-inspired green monochrome, no affiliation |
| `commodore-64` | dark | Retro — Commodore 64-inspired blue screen, no affiliation |
| `hercules-amber` | dark | Retro — Hercules-card-inspired amber monochrome |
| `vt220-green` | dark | Retro — DEC VT220-inspired green phosphor, no affiliation |

### Readability validation

Every built-in's default foreground/background pair is checked against a
minimum WCAG perceptual contrast ratio at build/test time. OdyTTY uses a floor
of **4.0** — just under the WCAG AA 4.5 threshold so that faithful community
palettes (Solarized in particular sits right at the boundary: ~4.1 light,
~4.75 dark) keep their authentic values rather than being silently retuned.

This is a *library-authoring* floor, not a render-time guarantee. Per-user
render-time contrast enforcement — a configurable minimum that lifts low-contrast
text from *any* app or theme — is available via `ODYTTY_MIN_CONTRAST` /
`min_contrast =` in `odytty.conf`. It builds on the same contrast helper
(`theme::contrast_ratio`) used to validate the library here.

### Attribution and licensing

OdyTTY-original themes and retro-inspired themes carry no external attribution.
Community themes are ported from their published sources; the table below lists
each source and its licensing posture as stated in the theme file headers.
"Published palette" means the palette was released publicly by its author(s)
without an explicit open-source license declaration in the theme header. MIT
ports are used and adapted under the MIT license.

| Theme(s) | Source | Notes |
| --- | --- | --- |
| `plain`, `odyssey`, all `odyssey-*` | OdyTTY original | Original design |
| `amber-crt`, `green-phosphor`, `hercules-amber`, `dos-cga` | OdyTTY original | Original retro-inspired designs |
| `catppuccin-mocha`, `catppuccin-latte` | Catppuccin | Published palette |
| `dracula` | Dracula (Zeno Rocha et al.) | Published palette |
| `gruvbox-dark` | Gruvbox (Pavel Pertsev) | Published palette |
| `monokai` | Monokai (Wimer Hazenberg) | Published palette |
| `nord` | Nord (Arctic Ice Studio) | Published palette |
| `one-dark` | One Dark (Atom) | Published palette |
| `solarized-dark`, `solarized-light` | Solarized (Ethan Schoonover) | Published palette |
| `tokyo-night` | Tokyo Night (enkia) | Published palette |
| `ayu-light`, `ayu-mirage` | Ayu (dempfi) | MIT, ported; no endorsement implied |
| `everforest-dark`, `everforest-light` | Everforest (sainnhe) | MIT, ported; no endorsement implied |
| `github-dark`, `github-light` | GitHub Primer (GitHub) | MIT, ported; GitHub is a trademark, no affiliation |
| `gruvbox-light` | Gruvbox (morhetz) | MIT, ported; no endorsement implied |
| `iceberg-dark` | Iceberg (cocopon) | MIT, ported; no endorsement implied |
| `kanagawa` | Kanagawa (rebelot) | MIT, ported; no endorsement implied |
| `night-owl` | Night Owl (Sarah Drasner) | MIT, ported; no endorsement implied |
| `oceanic-next` | Oceanic Next (Dmitri Voronianski) | MIT, ported; no endorsement implied |
| `one-light` | One Light (Atom / GitHub) | MIT, ported; no endorsement implied |
| `palenight` | Palenight (Astorino) | MIT-class, ported; no endorsement implied |
| `papercolor-light` | PaperColor (NLKNguyen) | MIT, ported; no endorsement implied |
| `rose-pine`, `rose-pine-dawn` | Rose Pine | MIT, ported; no endorsement implied |
| `tokyo-night-day` | Tokyo Night Day (enkia) | MIT, ported; no endorsement implied |
| `zenburn` | Zenburn (Jani Nurminen) | Freely ported; no endorsement implied |
| `apple-ii-green` | Inspired by Apple II display | Apple is a trademark; no affiliation or endorsement |
| `commodore-64` | Inspired by Commodore 64 display | Commodore is a trademark; no affiliation or endorsement |
| `ibm-5151` | Inspired by IBM 5151 display | IBM is a trademark; no affiliation or endorsement |
| `vt220-green` | Inspired by DEC VT220 display | DEC and VT220 are trademarks; no affiliation or endorsement |

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
