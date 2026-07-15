# OdyTTY theme files

OdyTTY themes are full appearance profiles: the default foreground/background,
the window clear color, the complete 16-color ANSI palette, and a set of
semantic role colors (cursor, selection, search highlight, plus reserved
border/inactive). A theme is selected with the `theme` setting
(`ODYTTY_THEME` / `theme =` in `odytty.conf`).

There are two kinds of theme:

- **Built-in themes**, selected by name — `odyssey` is the fresh-install
  default, while `plain` remains available as the pixel-identical pre-theme
  appearance plus a curated library of OdyTTY-original and community themes
  (see [Built-in theme
  library](#built-in-theme-library)).
- **User theme files**, written in the dependency-free theme file format
  described below and dropped into your theme directory (or referenced by path).

## Contents

- [Selecting a theme](#selecting-a-theme)
- [In-app theme tools](#in-app-theme-tools)
- [Built-in theme library](#built-in-theme-library)
- [File format](#file-format)
- [Example](#example)
- [How a selected theme is presented](#how-a-selected-theme-is-presented)
- [Precedence with running applications](#precedence-with-running-applications)

## Selecting a theme

```conf
# odytty.conf
theme = odyssey            # a built-in name
# theme = system           # follow the OS dark/light appearance
# theme = solarized        # a user theme: <config>/odytty/themes/solarized.theme
# theme = /path/to/my.theme  # a user theme by absolute/relative path
```

Resolution order for the `theme` value:

1. The special value `system` is a config alias — not a built-in name or a file.
   It turns on OS dark/light following: OdyTTY selects `os_theme_dark` (default
   `odyssey`) when the OS reports a dark appearance and `os_theme_light` (default
   `odyssey-light`) when it reports light. This is resolved before the steps
   below.
2. Otherwise, if it matches a built-in name (any theme in the [library](#built-in-theme-library)),
   the built-in is used.
3. Otherwise, if it looks like a path (contains `/` or ends in `.theme`), that
   file is read directly.
4. Otherwise it is looked up in the user theme directory as
   `<name>.theme`, then `<name>`.

If the value resolves to nothing, or the file cannot be read, OdyTTY falls back
to the default `odyssey` theme and logs a warning. **A bad or missing theme
value never prevents startup.**

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

## In-app theme tools

You can browse, preview, and create themes without editing files by hand.

### Theme Picker

The Theme Picker (default `Ctrl+Shift+H`, also reachable from the right-click
menu) lists every built-in and user theme with a live preview, so you can scroll
through the [library](#built-in-theme-library) and apply a theme by selecting
it. Each entry's light/dark label is derived from its background luminance (see
[File format](#file-format)). Press `B` inside the picker to open the Theme
Builder on the highlighted theme.

### Theme Builder

The Theme Builder (default `Ctrl+Shift+B`) is a no-file way to author user
themes. You can clone an existing theme, edit its foreground/background, palette,
and role colors, or generate a starting palette from a seed color, then save the
result. Saving writes `<theme_dir>/<name>.theme` in exactly the
[file format](#file-format) documented below — so a builder-made theme is an
ordinary user theme file you can keep editing by hand. On save, role colors are
snapped to meet the WCAG AA 4.5 contrast target.

See [docs/keybindings.md](keybindings.md) for the full set of default chords and
how to rebind them.

## Built-in theme library

OdyTTY ships a curated library of built-in themes, selectable by name with no
file needed. Every built-in is authored in the theme file format and loaded
through the same parser as a user theme — there is no privileged construction
path — so the file format is exercised by the library on every startup.

### Theme families

**OdyTTY original** — `plain`, `odyssey`, and the `odyssey-*` variants are
original themes designed for OdyTTY's public visual identity. The `odyssey` name
comes from OdysseyOS, a companion Linux From Scratch system, but these themes are
built into OdyTTY and do not require that system. `odyssey` is the fresh-install
default; `plain` reproduces the historical xterm default palette byte-for-byte
and remains available as an explicit compatibility choice. The `odyssey-*`
variants span a wide range of moods across dark and light appearances —
deep-space and interstellar cold, warm atmospheric, cool natural, cosmic nebula,
natural greens, vivid accents, and light daylight companions — and carry the
strongest OdyTTY visual identity. See the [library table](#built-in-theme-library)
below for the full roster.

**Community** — themes ported from widely-used open-source color-scheme
palettes, covering both dark and light sides (Solarized, Gruvbox, Nord, Dracula,
Tokyo Night, Catppuccin, Everforest, Rose Pine, and more). Sources and licenses
are listed in the [attribution table](#attribution-and-licensing) below; every
name appears in the [library table](#built-in-theme-library).

**Retro / phosphor** — eight themes inspired by historical display hardware:
green-phosphor and amber monochrome variants, an Apple II-inspired green phosphor,
the Commodore 64 blue-on-purple character screen, and a DOS/CGA sixteen-color text
palette tuned to the canonical ANSI hue angles. These use deliberately narrow
ANSI palettes that approximate the look of the original hardware while meeting the
library's 4.0 contrast floor. Vendor and platform names that appear in theme
titles are trademarks of their respective owners; OdyTTY has no affiliation with
or endorsement from any of those vendors.

| Name | Appearance | Family |
| --- | --- | --- |
| `plain` | dark | Pixel-identical to the pre-theme look |
| `odyssey` | dark | OdyTTY original default |
| `odyssey-noir` | dark | OdyTTY original (deep, low-key) |
| `odyssey-light` | light | OdyTTY original (light) |
| `odyssey-aurora` | dark | OdyTTY original (high-contrast) |
| `odyssey-deepspace` | dark | OdyTTY original (near-black interstellar void with cold starlight accents) |
| `odyssey-nebula` | dark | OdyTTY original (magenta and violet emission clouds with teal stellar accents) |
| `odyssey-solar` | dark | OdyTTY original (warm amber solar flare over a dark sunspot field) |
| `odyssey-abyss` | dark | OdyTTY original (deep teal hadal voyage with bioluminescent cyan) |
| `odyssey-ember` | dark | OdyTTY original (banked coals and forge orange against charcoal) |
| `odyssey-glacier` | dark | OdyTTY original (polar blue ice with cold high-legibility contrast) |
| `odyssey-meridian` | dark | OdyTTY original (warm parchment text on indigo twilight) |
| `odyssey-voyager` | dark | OdyTTY original (muted expedition greens with logbook warmth) |
| `odyssey-pulsar` | dark | OdyTTY original (neutron-star neon over near-black) |
| `odyssey-dawn-light` | light | OdyTTY original (violet-white morning companion to deep space) |
| `odyssey-sandstone-light` | light | OdyTTY original (sunlit dune and planetary-surface warmth) |
| `odyssey-graphite` | dark | OdyTTY original (minimal neutral graphite focus mode) |
| `odyssey-fathom` | dark | OdyTTY original (deep-ocean teal over near-black abyss) |
| `odyssey-harbor` | dark | OdyTTY original (cool naval blue with clear high-legibility text) |
| `odyssey-ion` | dark | OdyTTY original (electric indigo-violet over deep space) |
| `odyssey-orchard` | dark | OdyTTY original (cultivated greens on dark loam) |
| `odyssey-volcanic` | dark | OdyTTY original (ember warmth and ash over basalt) |
| `odyssey-cloud-light` | light | OdyTTY original (cool cloud-white with slate-blue text) |
| `odyssey-coral-light` | light | OdyTTY original (warm coral daylight companion) |
| `odyssey-mist-light` | light | OdyTTY original (soft misted green-grey morning) |
| `odyssey-twilight` | dark | OdyTTY original (indigo dusk with magenta and violet afterglow) |
| `odyssey-verdant` | dark | OdyTTY original (deep evergreen canopy over forest-floor dark) |
| `odyssey-quasar` | dark | OdyTTY original (brilliant cyan-blue jet over a near-black void) |
| `odyssey-meadow-light` | light | OdyTTY original (sunlit spring-green meadow companion) |
| `odyssey-parchment-light` | light | OdyTTY original (warm aged-parchment daylight with ink-brown text) |
| `odyssey-tidepool` | dark | OdyTTY original (teal-aqua tidepool over deep coastal dark) |
| `odyssey-moss` | dark | OdyTTY original (olive and moss greens on forest-floor dark) |
| `odyssey-rosewood` | dark | OdyTTY original (warm rose and wine over a dark grain) |
| `odyssey-slate` | dark | OdyTTY original (cool steel-blue neutral focus mode) |
| `odyssey-blossom-light` | light | OdyTTY original (soft rose-blossom daylight companion) |
| `odyssey-linen-light` | light | OdyTTY original (warm neutral linen daylight with ink text) |
| `odyssey-garnet` | dark | OdyTTY original (deep crimson and wine-red over a maroon-black grain) |
| `odyssey-sepia` | dark | OdyTTY original (warm sepia-brown monochrome focus mode) |
| `odyssey-cobalt` | dark | OdyTTY original (electric royal-blue jet over deep cobalt navy) |
| `odyssey-lilac-light` | light | OdyTTY original (soft lavender daylight with violet accents) |
| `odyssey-pearl-light` | light | OdyTTY original (cool neutral pearl-grey daylight) |
| `odyssey-apricot-light` | light | OdyTTY original (warm apricot-peach daylight companion) |
| `odyssey-chartreuse` | dark | OdyTTY original (yellow-green chartreuse accent on deep olive-black) |
| `odyssey-violet` | dark | OdyTTY original (amethyst violet accent on deep indigo-black) |
| `odyssey-fuchsia` | dark | OdyTTY original (vivid magenta-pink accent on deep magenta-black) |
| `odyssey-butter-light` | light | OdyTTY original (warm golden-yellow honey daylight companion) |
| `odyssey-sage-light` | light | OdyTTY original (muted herbal sage-green daylight companion) |
| `odyssey-slate-light` | light | OdyTTY original (cool blue-slate daylight companion) |
| `odyssey-amber` | dark | OdyTTY original (warm golden amber on deep warm-dark) |
| `odyssey-default` | dark | **Shipped default** (deep tropical green canopy on forest-floor dark); also reachable via the `odyssey-jungle` alias |
| `odyssey-orchid` | dark | OdyTTY original (rich orchid purple-pink on deep mauve-dark) |
| `odyssey-seafoam-light` | light | OdyTTY original (soft seafoam-green daylight companion) |
| `odyssey-indigo` | dark | OdyTTY original (deep indigo blue-violet between the blue and purple clusters) |
| `odyssey-raspberry` | dark | OdyTTY original (deep berry raspberry between magenta-pink and rose-red) |
| `odyssey-citrus-light` | light | OdyTTY original (fresh lime-citrus daylight companion) |
| `odyssey-mauve-light` | light | OdyTTY original (dusty mauve daylight companion between lavender and rose) |
| `odyssey-terracotta` | dark | OdyTTY original (warm clay and terracotta over a near-black hearth ground) |
| `odyssey-harvest` | dark | OdyTTY original (golden wheat harvest on deep autumn dark) |
| `odyssey-lagoon` | dark | OdyTTY original (bright cyan-azure lagoon surface over deep teal-blue depths) |
| `odyssey-clover-light` | light | OdyTTY original (spring green clover daylight companion) |
| `odyssey-midnight` | dark | OdyTTY original (maximum-contrast blue-black midnight with stark blue-white text) |
| `odyssey-sienna-light` | light | OdyTTY original (warm sienna-cream daylight companion with terracotta accents) |
| `odyssey-periwinkle-light` | light | OdyTTY original (cool blue-violet periwinkle daylight companion) |
| `odyssey-pine` | dark | OdyTTY original (muted forest-pine dark with sage-green text) |
| `odyssey-eclipse` | dark | OdyTTY original (umbral indigo with a corona-gold ring accent) |
| `odyssey-comet` | dark | OdyTTY original (near-black void with an icy blue-white tail) |
| `odyssey-obsidian` | dark | OdyTTY original (cool volcanic glass with teal and violet glints) |
| `odyssey-jade` | dark | OdyTTY original (deep green mineral field with warm amber highlights) |
| `odyssey-basalt` | dark | OdyTTY original (warm charcoal stone with ember-orange accents) |
| `odyssey-fjord` | dark | OdyTTY original (deep blue-green inlet with cool crisp accents) |
| `odyssey-mangrove` | dark | OdyTTY original (dark forest teal with root-brown and leaf accents) |
| `odyssey-nocturne` | dark | OdyTTY original (deep blue-violet night with moonlit silver accents) |
| `odyssey-quartz-light` | light | OdyTTY original (cool pale grey-white daylight with crisp ink) |
| `odyssey-marble-light` | light | OdyTTY original (warm ivory daylight with terracotta and olive ink) |
| `odyssey-dune-light` | light | OdyTTY original (sandy warm parchment with sun-baked accents) |
| `odyssey-fern-light` | light | OdyTTY original (soft green-tinted daylight with woodland accents) |
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
| `font_family` | Optional font-family hint (schema metadata; not applied by theme selection). |
| `font_size` | Optional font-size hint in px (schema metadata; not applied by theme selection). |
| `visual` | Bundled visual-effect profile metadata: `off`, `ambient`, or `scanlines`. `ambient` and `scanlines` are aliases for *each other* — both map to the `Ambient` effect, a faint static scanline wash applied to cell backgrounds only (glyphs untouched), which is distinct from the separate CRT post-process scanline pass (the `crt` / `crt_scanline_*` settings). Theme selection does not auto-apply this field. |

`appearance`, `font_family`, `font_size`, and `visual` are parsed, validated, and
round-tripped, but none are projected into the running theme:

- The light/dark label shown by `--list-themes` and the in-app Theme Picker is
  derived at runtime from the theme's background **relative luminance** — a
  background luminance above `0.18` is treated as light — not from the file's
  `appearance` field, which is retained as metadata only.
- The appearance column in the [library](#built-in-theme-library) table agrees
  with the file because each built-in's authored `appearance` matches its
  background luminance.
- The font and visual fields are likewise kept as schema metadata rather than
  auto-applied when the theme changes.

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

## How a selected theme is presented

Two settings change how the active theme is rendered:

- **`themed_ui_roles`** (default on) lets the theme's semantic role colors drive
  OdyTTY's own overlay and UI chrome, so the picker, settings panel, and other
  surfaces pick up the selected theme rather than a fixed palette.
- **Color-vision-deficiency adaptation** (`cvd_mode`, default off; `cvd_strength`,
  default `1.0`) applies OKLab daltonization to the theme's 16 ANSI slots plus
  the cursor, selection, and search colors at render time, without altering the
  theme file. See [docs/accessibility.md](accessibility.md) for the CVD modes and
  the related readability controls (`min_contrast`, `focus_dim`, bell).

## Precedence with running applications

Application-driven color changes still win over the theme. A program that sets
palette entries via `OSC 4` (or the default fg/bg via `OSC 10/11`) overrides the
theme's colors for the lifetime of that session; the theme provides the
baseline those overrides start from. Selecting a new theme re-establishes the
baseline.
