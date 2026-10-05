# Text Shaping Roadmap

Companion to the shaping summary in [`docs/features.md`](features.md). This document states
the current model, its measured limit, the standing scope boundaries, and the
work that remains possible without weakening terminal semantics.

## The model

OdyTTY's terminal model keeps logical source owners on a fixed cell grid. That
invariant is not negotiable: cursor addressing, selection, search, copy,
scrollback, and transcript export all address the grid by cell, and every one
of them has to stay exact regardless of how a cell's glyph is drawn.

Generated wide-glyph wrap padding has no logical scalar. Its provenance
survives history and snapshot storage, so resizing does not inject spaces into
search, copy, or export. Typed spaces remain logical source text. This does not
change glyph placement.

Each source owner now retains up to seventeen scalars. Ordinary leading marks and
extensions beyond that bound start new owners without synthetic source text.
Controls, cursor movement, edits, hard breaks, and resize terminate extension;
SGR preserves it. Thai/Lao consonant plus SARA AM extends to a two-cell owner.
Tone marks, Tibetan subjoined letters, and pre-base vowels retain their prior
widths. Northern and southern Indic, Sinhala, Khmer, Myanmar, Chakma, Javanese,
Grantha, and Tai Tham spacing
signs and linked consonants share bounded two-cell owners using Unicode 17
properties. ZWJ preserves virama/stacker links; Chakma U+11134 keeps prior
separate owners with ZWJ. A Tai Tham spacing sign before SAKOT stays in the
owner. ZWNJ ends consonant joining. Gurmukhi,
Tamil, Sinhala, Chakma, and Grantha width units can cross extended-grapheme boundaries.
Exact VS16 keycaps omit an unmapped selector only from color shaping so an unmapped
selector cannot block a present base-plus-enclosing-keycap ligature. Logical
source ranges, width and cache identity retain the selector. Capability probes
and live rendering share this seam; flag, modifier and ZWJ inputs are unchanged.

Listed VS16 bases and recognized emoji modifier, keycap, flag, and RGI ZWJ
sequences use two-cell owners. Northern and southern Indic, Sinhala, Khmer, Myanmar, Thai, Lao, and Tibetan
owners are shaped with the font (see Complex Indic and Brahmic shaping
below); Chakma, Javanese, Grantha, and Tai Tham shaping remains pending.
Unattached width-zero format controls and selectors occupy zero columns and
are still not retained. They extend source text when an eligible owner exists.

Shaped presentation is layered on top of that grid as anchored overlay spans
(`LigatureRun`) rather than by letting shaping change the grid itself. A run
covers a contiguous span of source cells; the shaped glyphs it produces are
clipped to that span's pixel box and never advance into a neighboring cell.
Shaped advances never move terminal columns -- a ligature or contextual
substitution can change what is drawn, never which column it is drawn in or
what character copy/paste reports for that cell.

The consequence is a real one: OdyTTY's shaping is a presentation overlay on a
monospace-cell grid, not a full text-shaping engine that can reflow glyph
advances or cell counts. That covers ASCII contextual ligatures, curated
operator ligatures, Arabic joining forms in logical cell order, and static
color glyphs correctly. It does not cover scripts whose correct rendering
requires reordering or reshaping across cell boundaries (see Standing scope
boundaries, below).

## Current support boundary

This matrix is the same support statement carried by [`docs/features.md`](features.md):

| Surface | Current support | Standing position |
| --- | --- | --- |
| Latin and programming operators | ASCII `calt`+`liga`, a curated non-ASCII operator allowlist, opt-in `ss01`/`ss02` overlays, and an opt-in alternate zero (`zero`) | More curated operators and named, bounded legibility features are candidates within the current overlay model; open-ended stylistic sets and raw feature tags are not |
| Arabic | Contextual joining forms in logical left-to-right cell order, or shaped right to left in display order while `bidi_reorder` is on; harakat ride their base into the joining run with the font's mark positioning | More joining-script coverage is a candidate; Arabic marks outside the supported harakat set, and harakat the font does not map, keep the monochrome path |
| Bidirectional layout | Opt-in `bidi_reorder` (off by default): right-to-left runs drawn in display order on the primary screen with a left-to-right paragraph level; cells, cursor addressing, selection, copy, search, and protocol values stay logical | The alternate screen, right-to-left paragraph levels, and complex-script shaping are not reordered |
| Northern and southern Indic shaping | With `ligatures` on, Devanagari, Bengali, Gurmukhi, Gujarati, Odia, Tamil, Telugu, Kannada, and Malayalam owners are shaped with the font (conjuncts, reph, below-base and pre-base forms, ZWJ/ZWNJ requests) and drawn inside the owner's one or two cells, centered, or scaled to a 0.6 floor and then clipped | Owners keep the per-cell path when no font maps every scalar; the cursor block redraws per-cell glyphs; shaping never crosses owners |
| Khmer/Myanmar shaping | With `ligatures` on, Khmer and Myanmar owners are shaped with the font (coeng stacks, below-base ro, pre-base and split vowels, medial ra, kinzi, stacker forms, ZWJ requests) and fitted inside the owner's cells by the same rule | Same limits as Indic owner shaping |
| Thai/Lao/Tibetan shaping | With `ligatures` on, Thai, Lao, and Tibetan owners are shaped with the font (SARA AM decomposition, tone-mark and descender alternates, stacked tone and vowel marks, precomposed Tibetan subjoined stacks) and fitted inside the owner's cells by the same rule | Same limits as Indic owner shaping; widths are unchanged, so tone marks and Tibetan subjoined letters stay in their one-cell owners |
| Sinhala shaping | With `ligatures` on, Sinhala owners are shaped with the font (ZWJ conjunct requests, pre-base and split vowels) and fitted inside their one or two cells by the same rule | Shaping never crosses owners; ZWNJ breaks ownership; widths and logical copy stay unchanged |
| Other complex Indic/Brahmic shaping | Not supported | Chakma, Javanese, Grantha, and Tai Tham owners keep the per-cell path |
| Northern Indic terminal widths | Devanagari, Bengali, Gurmukhi, Gujarati, and Odia spacing signs and virama-linked consonants share bounded two-cell owners | Terminal width ownership is distinct from Unicode segmentation and font-backed shaping |
| Southern Indic terminal widths | Tamil, Telugu, Kannada, and Malayalam spacing signs and virama-linked consonants share bounded two-cell owners | Tamil width units can cross grapheme boundaries; font-backed shaping remains within each owner |
| Sinhala terminal widths | Dependent spacing signs and virama-linked consonants share bounded two-cell owners, with ZWJ preserving the link and ZWNJ breaking it | Sinhala terminal width units can cross grapheme boundaries; font-backed shaping stays inside each owner |
| Khmer/Myanmar terminal widths | Dependent spacing signs and coeng/invisible-stacker-linked consonants share bounded two-cell source owners; Khmer U+17A4 and U+17D8 occupy one cell | ZWJ preserves linking, ZWNJ breaks it; font-backed shaping is the separate Khmer/Myanmar shaping row |
| Additional measured script widths | Chakma, Javanese, Grantha, and Tai Tham spacing signs and measured linked consonants share bounded two-cell owners | Chakma U+11134 links directly but keeps separate owners with ZWJ; Javanese/Grantha viramas alone keep one cell; font-backed complex-script reordering remains unsupported |
| Emoji cluster rendering | Unicode 17 listed VS16 bases, fully qualified RGI ZWJ sequences, modifier-base skin tones, VS16 keycaps, and regional-indicator pairs share bounded two-cell source owners | Color glyphs remain font-dependent; VS15 does not demote, standalone regional indicators and keycaps without VS16 stay one cell, and non-RGI joins keep separate owners |
| SVG-in-OpenType | SVG documents draw glyphs that no bitmap, COLR v0, or COLR v1 source covers, in the same one- or two-cell color slot | Documents over the size, node, nesting, or reference-expansion limits, and documents with patterns or stylesheet `url(` references, use monochrome fallback; no file, network, or embedded image loads, and SVG text is not drawn |

## What the overlay model supports

- **Shaping-run infrastructure.** The presentation shaper now groups cells
  into shaping runs by grapheme cluster, with a byte-to-column map that anchors
  each shaped glyph back to the source cell(s) it came from, and compatible-run
  boundary detection. Runs break at wide continuations, hidden cells,
  color-glyph coverage, cells carrying combining marks, bold/italic face
  changes, and Latin-vs-Arabic shaping-kind changes, so those categories never
  merge into a shaped span. Live overlay eligibility covers ASCII-graphic
  bases, a curated allowlist of common non-ASCII programming operators and
  arrows (`SHAPING_OPERATOR_ALLOWLIST`), and Arabic joining bases. Plain ASCII
  content without allowlisted scalars or Arabic letters stays byte-identical to
  the pre-allowlist path, pinned by a differential test. Optional stylistic
  sets are limited to explicit `ss01`/`ss02` settings (off by default); open-
  ended `ssXX` remains deferred.
- **Static color glyphs (COLR/CPAL v0).** The color-glyph path renders static
  COLR/CPAL v0 layer compositions in addition to the existing bitmap-strike
  formats, including stock Windows Segoe UI Emoji, which previously fell back
  to the monochrome path. See [`docs/features.md`](features.md) for the full color-emoji
  support statement.
- **COLR v1 Paint graphs.** The same color-glyph atlas now accepts v1-only
  glyphs through Fontations' guarded graph traversal. Solid fills, gradients,
  transforms, clips, and composites rasterize into premultiplied RGBA after
  bitmap and v0 sources decline the glyph, preserving both established paths.
- **Extended ligature coverage beyond ASCII.** Landed as the curated allowlist
  above, not an open feature-tag surface.
- **Latin `liga` alongside `calt`.** Eligible Latin/operator runs enable both
  OpenType tags when programming ligatures are on. The off/on differential
  still emits overlays only where newly enabled features change glyphs, so
  plain content without substitutions stays byte-identical to the scalar path.
- **Optional `ss01` / `ss02`.** Explicit settings (`ss01` / `ss02`, env
  `ODYTTY_LIGATURE_SS01` / `ODYTTY_LIGATURE_SS02`), both off by default, apply
  only while programming ligatures are enabled. No other `ssXX` tags are
  exposed.
- **Alternate zero (`zero`).** The named legibility control `font_zero` (env
  `ODYTTY_FONT_ZERO`), off by default, applies the body font's OpenType `zero`
  feature with ligatures on or off. Each body face resolves its `zero`
  substitution for `0` once when the control is applied, and the scalar atlas
  path draws that glyph. Contextual runs set `zero` identically in both shaping
  passes, so the control never creates an overlay and a shaped `0` matches the
  scalar one. Cell metrics are unchanged, and a face without the feature renders
  byte-identically, pinned by atlas fixtures on the bundled JetBrains Mono
  (which has `zero`) and Victor Mono (which does not).
- **Arabic contextual joining forms.** Compatible Arabic runs are shaped with
  `Script::Arabic` under **logical left-to-right cell order**, or right to
  left in display order while `bidi_reorder` is on (see Bidirectional layout).
  OpenType init/medi/fina/isol (and length-changing joining ligatures such as
  lam-alef) become `LigatureRun` overlays clipped to their source-cell spans.
  Selection, copy, search, and cursor addressing still report the logical
  characters in cell order. In logical order the connecting strokes of
  adjacent letters face away from each other, so a word draws as one connected
  shape only with `bidi_reorder` on. Each joined glyph is drawn from its own
  cell's left edge, so strokes connect only when the text font's joining forms
  advance exactly one cell, as in a monospace face; a proportional face loaded
  through the `font` path setting, which is not checked for monospace, draws
  gaps between joined letters. When the active text font has no Arabic
  coverage, the shaper emits no overlay and the ordinary per-cell path remains
  (no invented tofu).
- **Arabic harakat inside joining runs.** An Arabic joining letter whose
  retained marks are all Arabic harakat (the Arabic and Arabic Extended-A
  nonspacing marks, except U+08CA through U+08D2, which the shaping engine's
  character data does not treat as transparent) stays in its joining run, so
  the letter takes its contextual form and its neighbors keep theirs. The
  font's composition (for example shadda with fatha) and mark-to-base
  positioning apply: each mark is drawn at its shaped offset from its base,
  clipped to the run's cells, and is not drawn a second time by the monochrome
  combining path. Within a run of two or more Arabic cells every marked letter
  draws this way, including one whose form joining leaves unchanged; a marked
  letter with no adjacent Arabic letter keeps the monochrome path. A mark
  without an attachment is drawn from its base's advance, which matches the
  monochrome placement when the letter is one cell wide. A marked cell whose
  marks the text font does not map, and any other combining mark on an Arabic
  letter, keep the monochrome combining path and split the run there. Copy,
  search, selection, and cursor addressing are unchanged. Mark ink stays
  within the run's cells horizontally and within the glyph slot's overflow
  margin vertically.

## Measured extent

The independent 2026-08-16 review included a `ucs-detect` run covering 85
languages. Its aggregate check pass rate was 81.2%. Failures appeared in 22
language cases, all Brahmic or derived from Southeast Asian Brahmic scripts.
The run reported no failures for its Latin, Cyrillic, Greek, CJK, Hebrew, or
non-conjunct Arabic cases.

That result measures the tested corpus on that machine. It is not a claim that
81.2% of languages are supported, that every unfailed script is complete, or
that the renderer implements Unicode shaping generally. It does locate the
observed boundary in the same class predicted by the model: scripts that need
conjunct formation, glyph reordering, or cluster reassembly across logical
cells.

## Standing scope boundaries

### Bidirectional layout

Bidirectional display reordering is a bounded, opt-in setting:
`bidi_reorder` (Settings > Rendering > Bidirectional text, the right-click
**Reorder Right-to-Left Text** toggle, or `ODYTTY_BIDI_REORDER=on`), off by
default. While it is on, the primary screen draws right-to-left runs (Hebrew,
Arabic) in display order with a left-to-right paragraph level. With it off,
right-to-left input is stored and drawn in logical cell order, exactly as
before; Arabic joining forms are then shaped within that order. The menu
toggle applies to the running window; Save in Settings persists it.

The setting changes presentation only. Cells, cursor addressing and movement,
selection endpoints, copy, search results, scrollback export, and every
terminal protocol value stay logical. Plain limits: the alternate screen
(full-screen programs) is never reordered; the paragraph level is always left
to right, with no right-to-left paragraphs; shaped complex-script owners stay
on their own cells; a mirrored character without a Unicode mirroring counterpart draws
unmirrored; block-selection export is not reordered or specially handled;
image placements are not reordered.

The plan comes from a headless module, `src/core/bidi`: for one
wrapped logical line it resolves UAX #9 levels with the paragraph level forced
to left to right, applies the line rules to each physical row, and returns
reversible owner-to-visual-span and visual-column-to-owner maps with
mirrored-glyph flags. Paragraphs over its owner, byte, or row cap get the
complete identity layout. It passes every case of the Unicode 17.0.0
BidiCharacterTest.txt and BidiTest.txt files, with a committed subset
asserted in the test suite. Its bidi class, bracket-pair, and mirroring data
is Unicode 17.0.0, the same version as the width tables, generated from the
Unicode Character Database by `scripts/unicode-bidi-data.py`. Bidi format
controls (embeddings, overrides, isolates, LRM, RLM, and ALM) can be passed as
width-0 owners that keep their logical position, take part in level
resolution, and cover no visual column.

The renderer draws a snapshot through those plans. It treats each
run of soft-wrapped rows as one paragraph and draws every cell at its visual
column; a wide cell keeps its lead and continuation cells together. A mirrored
character in right-to-left text draws its Unicode 17.0.0 Bidi_Mirroring_Glyph
counterpart. Each glyph on a reordered row is clipped to its own visual cells.
Contextual shaping runs split wherever the resolved level changes, so no
ligature crosses a direction boundary. Arabic joining runs are shaped right to
left, and each joined glyph is placed on the visual cells of its source
characters. Left-to-right ligatures are not formed inside right-to-left text.
Pixel fixtures use a project-authored font. They compare each mixed-direction
line against the unchanged renderer drawing the hand-ordered visual string,
covering Hebrew with Latin and digits, a mirrored bracket pair, wide cells, a
ligature across a direction boundary, Arabic lam-alef joining, and color glyphs.
A paragraph that begins above the visible rows is resolved together with
the soft-wrapped rows above it, from scrollback or the screen above a scrolled
viewport, up to the plan's row and owner caps; a longer paragraph keeps the
identity layout, as any over-cap paragraph does. A mirrored character with no
mirroring counterpart (U+2211, for example) draws unmirrored.

While the setting is on, the live frame draws its content grid in
display order, and each pane of a split tab plans its own map. Tab-bar and
rail cells stay in logical order, and so does any row an overlay draws text
over. The cursor stays on its logical cell and draws at that cell's visual
column. The pointer over a screen column addresses the logical cell drawn
there, in the single pane or the focused split pane, so a drag selects
logical cells, which can appear as separate segments on screen when the
selection crosses a direction boundary. Mouse reports to applications name
that logical cell in every cell encoding, and an SGR-pixel report moves by
whole cells onto it while keeping its offset inside the cell. Hyperlink hover
follows the logical cell under the pointer. The input method candidate window
anchors at the screen column the cursor cell is drawn at. The single-pane
frame cache key includes the placement, so a placement change alone redraws.
Selection and search highlights draw on the visual cells of their logical
cells. The cursor slide, trail, follower, and aura move between the screen
cells the cursor is drawn at, so a move on a reordered line animates as the
same screen move on an unreordered one. A frame whose overlay writes text into
the cursor's row draws that row in logical order and snaps the cursor effects
instead of gliding. The open-modifier underline, link hover, and button chips
address logical cells through the pointer; underlines and chip fills draw on
the visual cells of their logical cells, while a chip pill cap or the click
hint writes text, so its row draws in logical order. Copy, search results, and
cursor addressing stay logical. The map is planned for every frame from the
rows on screen and the paragraph above them, so after a width change, at any
history scroll position, in a session restored from a snapshot, and for
output delivered in any split of reads, cells are placed exactly as in a
terminal that shows the same rows directly. Scrollback export stays in
logical order. Image placements are not part of the plan. The plan is not
cached between frames.

### Complex Indic and Brahmic shaping

Correct conjuncts can require several source characters to form one cluster,
glyphs to reorder around the cluster, and marks to attach at positions that do
not correspond to their source cells. The retained width owner makes that
tractable without a per-glyph cell mapping: every source scalar of a conjunct,
reph, or split vowel already lives in one owner of one or two cells, so the
owner is the shaping unit and its cells are the only place its glyphs may
draw. Editing, erase, resize, reflow, scrollback, selection, search, cursor
movement, snapshot, and export keep operating on owners and never see glyphs.

Northern and southern Indic owners (Devanagari, Bengali, Gurmukhi, Gujarati,
Odia, Tamil, Telugu, Kannada, and Malayalam) are
shaped that way while `ligatures` is on. Each owner is shaped on its own with
`harfrust` 0.8.4, the HarfBuzz project's Rust port; `swash`, which shapes the
Latin, Arabic, and emoji paths, does not form reph and mishandles ZWJ/ZWNJ
requests in these scripts. The face is the first that maps every scalar: a
`symbol_map` override, the style face, then the fallback chain and runtime
resolver. Shaped glyphs keep their pen positions; the run is centered in the
owner's span when it fits, otherwise scaled uniformly to fit down to a 0.6
floor, then clipped at the span edges. An owner with no covering face, a
pending runtime answer, a `.notdef` result, or no atlas room keeps the
per-cell path. A stray mark that starts an owner is shaped with a dotted
circle when the face maps U+25CC, and alone otherwise. The cursor block redraws per-cell glyphs, as it
does under a Latin ligature. Shaping never crosses owners. Display reordering
leaves left-to-right owners on their own cells.

Khmer and Myanmar owners take the same path: coeng stacks, below-base ro,
pre-base and split vowels, medial ra, kinzi, and stacker forms come from the
font's own shaping tables.

Thai, Lao, and Tibetan owners take it as well: SARA AM decomposition,
tone-mark and descender alternates, stacked tone and vowel marks, and
precomposed Tibetan subjoined stacks come from the font.

Sinhala owners use the same path for ZWJ conjunct requests and pre-base
and split vowels. The ZWNJ control keeps separate owners.

Licensed fixtures (OFL subsets of the Noto faces) carry HarfBuzz 14.5.1
references for all 49 northern and 33 southern Indic samples; all but one
match exactly. Bengali
ka, virama, ra shapes differently in `harfrust` 0.8.4, and the fixtures keep
that difference as an asserted known difference rather than a conformance
claim. Every Khmer, Myanmar, Thai, Lao, and Tibetan sample matches exactly. All eight Sinhala samples match exactly. Chakma, Javanese, Grantha,
and Tai Tham owners keep the per-cell path until their own
classifier stage and fixtures land.

### SVG-in-OpenType

SVG-in-OpenType glyphs rasterize through resvg into the same bounded one-cell
or two-cell color atlas slot used by bitmap, COLR v0, and COLR v1 sources, and
only for glyphs none of those sources draws. The logical grid does not change.
The glyph element's ink box is fitted into the slot the way COLR v1 glyphs are.

Documents are untrusted: the `SVG ` index is read with checked arithmetic; a
document is at most 1 MiB raw and after gzip decompression; XML is parsed with
DTDs refused and at most 20,000 nodes; nesting is at most 64 deep; `use`,
`href`, and `url(#id)` references may expand to at most 80,000 nodes, and
cycles are refused; documents with patterns or stylesheet `url(` references are
refused. No file, network, or embedded image is loaded, scripts and animation
have no effect, and SVG text is not drawn. Output is premultiplied RGBA no
larger than the slot. A refused or empty document keeps the monochrome
fallback, and a key that fails is remembered so it is not reparsed each frame.
Synthetic SVG-only fixtures cover these paths on Linux, macOS, and Windows.

## Tractable candidate work

Sequence-aware width uses bounded source ownership without cross-cell visual
reordering. Unicode 17 listed VS16 bases promote to two cells. Modifier-base
skin tones, VS16 keycaps, regional-indicator pairs, and fully qualified RGI
ZWJ sequences share two-cell owners. The color renderer uses the same owned
span; glyph coverage remains font-dependent. Unrecognized continuations start
separate owners. Other extended-grapheme sequences still need bounded width
rules before making a general cluster-width claim.

VS15 deliberately does not demote. Standalone regional indicators remain one
cell and keycaps without VS16 remain one cell. These choices differ from the
frozen Python wcwidth reference; they are explicit compatibility limits.

A Tai Tham medial or spacing vowel sign written before SAKOT (U+1A60) stays
inside its consonant's owner, so the stacked consonant joins that owner
instead of starting a new one. Other scripts still end consonant linking at a
retained spacing sign. Two frozen `ucs-detect` corpus rows keep three cells
where the wcwidth reference reports two: a Tai Tham sequence that starts with
the dependent vowel U+1A63 and a Malayalam sequence that starts with the chillu
U+0D7B. OdyTTY links a virama or stacker only back to a same-script consonant
in its linking table, which includes neither a dependent vowel nor a chillu
(a dead consonant), so each keeps a separate leading owner. These
are deliberate differences from the reference, not passes.

Other candidates that fit the anchored overlay model are:

- more operators added through the reviewed scalar allowlist;
- more explicit, opt-in OpenType features with bounded settings, rather than an
  unrestricted tag surface;
- further joining-script coverage only where it needs contextual
  substitution without visual reordering.

Each candidate must preserve logical cells, copy/search output, cursor columns,
wide-cell boundaries, and fallback behavior. A candidate moves to supported
only with differential tests proving those properties.

## Other deferred extensions

- **Open-ended stylistic sets** beyond the explicit `ss01`/`ss02` settings.
  The named alternate-zero control (`font_zero`) is the only legibility
  feature exposed; an unrestricted `ssXX` or raw feature-tag surface stays
  deferred.

## Sequencing rationale

The shaping-run infrastructure was sequenced before broader ligature coverage
and Arabic joining because both need the same grapheme-cluster and
byte-to-column substrate to anchor overlays correctly. Arabic joining followed
because it is a script-tagged feature application on that same overlay model in
logical cell order.

Sequence-aware width follows the cell-storage work because it changes cluster
ownership without requiring visual reordering. Owner-run shaping follows it,
because the owner bounds every glyph a cluster can draw. BiDi display order is
a separate opt-in plan over the same logical cells. SVG-in-OpenType is
independent of that sequence.
