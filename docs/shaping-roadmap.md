# Text Shaping Roadmap

Companion to the shaping summary in [`docs/features.md`](features.md). This document states
the current model, its measured limit, the standing scope boundaries, and the
work that remains possible without weakening terminal semantics.

## The model

OdyTTY's terminal model keeps one logical character per grid cell. That
invariant is not negotiable: cursor addressing, selection, search, copy,
scrollback, and transcript export all address the grid by cell, and every one
of them has to stay exact regardless of how a cell's glyph is drawn.

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
| Latin and programming operators | ASCII `calt`+`liga`, a curated non-ASCII operator allowlist, and opt-in `ss01`/`ss02` overlays | More curated operators and bounded, explicit font-feature settings are candidates within the current overlay model |
| Arabic | Contextual joining forms in logical left-to-right cell order; combining-marked cells stay on the monochrome path | More joining-script coverage that requires no visual reordering is a candidate; this is not bidirectional layout |
| Full Unicode bidirectional layout | Not supported | Outside the current overlay model. Correct support first requires line-level logical-to-visual mapping shared by rendering, hit testing, cursor movement, selection, damage tracking, and copy semantics |
| Complex Indic/Brahmic shaping | Not supported | Outside the current one-character-per-cell overlay model. Correct support requires grapheme-cluster ownership plus reordered glyph placement that remains reversible to logical cells |
| Emoji cluster rendering | VS15/VS16 presentation, flags, keycaps, skin tones, and common ZWJ clusters are reconstructed for the color-glyph renderer | Rendering support does not yet make grid width cluster-aware; sequence-aware width is tractable follow-up work |
| SVG-in-OpenType | Not supported; SVG-only glyphs use monochrome fallback | Planned for v0.17.0. It requires a bounded, non-networked SVG raster path and portable fixtures before enablement |

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
- **Arabic contextual joining forms.** Compatible Arabic runs are shaped with
  `Script::Arabic` under **logical left-to-right cell order** (explicitly not
  bidi reordering). OpenType init/medi/fina/isol (and length-changing joining
  ligatures such as lam-alef) become `LigatureRun` overlays clipped to their
  source-cell spans. Selection, copy, search, and cursor addressing still
  report the logical characters in cell order. Cells that carry combining
  marks - including Arabic harakat - still break runs and stay on the mono
  combining path; that is a stated limitation of this slice, not silent
  wrongness. When the active text font has no Arabic coverage, the shaper
  emits no overlay and the ordinary per-cell path remains (no invented tofu).

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

### Full Unicode bidirectional layout

Full BiDi is outside the current overlay model and has no partial implementation
roadmap. Right-to-left input is stored and drawn in logical cell order. Arabic
joining forms are shaped within that order; cells are not reordered into visual
reading order.

Correct support would require a line-level Unicode Bidirectional Algorithm pass
and a stable, reversible logical-cell-to-visual-position map. Rendering, hit
testing, cursor movement, selection, search highlighting, damage tracking,
wide-cell handling, and copy behavior would all have to use that map. Acceptance
would require Unicode BiDi conformance data plus mixed-direction terminal tests
covering isolates, numbers, cursor navigation, rectangular and linear
selection, reflow, scrollback, and logical-order copy. Until those prerequisites
exist together, partial visual reordering is rejected because it would make
what the user sees disagree with terminal addressing and copied text.

### Complex Indic and Brahmic shaping

Complex Indic/Brahmic shaping is outside the current one-character-per-cell
overlay model and has no approximate fallback claim. Correct conjuncts can
require several source characters to form one cluster, glyphs to reorder around
the cluster, and marks to attach at positions that do not correspond to their
source cells.

Support would require grapheme-cluster ownership in the terminal presentation
model and a reversible mapping between each cluster's logical source cells and
its reordered glyphs. The mapping would need to survive editing, erase, resize,
reflow, scrollback, selection, search, cursor movement, snapshot, and transcript
export. Acceptance would require script-specific shaping conformance fixtures
and cell-by-cell semantic tests across those operations. Adding isolated
OpenType script tags to the existing overlay is not sufficient.

### SVG-in-OpenType

SVG-in-OpenType is planned for v0.17.0 and is not a conflict with the cell
model. An SVG glyph can rasterize into the same bounded one-cell or two-cell
color atlas slot used by bitmap, COLR v0, and COLR v1 sources. The logical grid
does not need to change.

Enablement requires a bounded rasterizer with external resource loading,
network access, scripts, animation, and unbounded document expansion disabled;
checked document and raster-size limits; deterministic premultiplied-RGBA
output; cache and fallback behavior matching the other color sources; and
portable SVG-only fixtures exercised on Linux, macOS, and Windows. Until that
surface is implemented and tested, SVG-only glyphs use monochrome fallback.

## Tractable candidate work

Sequence-aware grid width for extended grapheme clusters is candidate work,
not part of the deferred model-level scope. The terminal currently asks
`unicode-width` for each codepoint without lookahead. Consequently VS16 does
not promote a text-default scalar from one column to two, VS15 does not demote
an emoji-default scalar from two columns to one, and a ZWJ emoji sequence can
consume the sum of its component widths instead of one cluster width. The color
renderer can still reconstruct and draw those sequences as one glyph, which is
why rendering support and width correctness must be stated separately.

Fixing width needs sequence-aware arithmetic and cell ownership, but no
cross-cell visual reordering. It is therefore sequenced after the cell and
scrollback storage work rather than grouped with BiDi or conjunct shaping.
Standalone regional indicators are a separate ecosystem decision:
`unicode-width` and Python `wcwidth` disagree on their scalar width. A regional
indicator pair currently totals two columns by independent scalar arithmetic,
not because the grid recognizes a flag cluster. The standalone disagreement is
recorded as a compatibility judgment call, not an OdyTTY defect.

Other candidates that fit the anchored overlay model are:

- more operators added through the reviewed scalar allowlist;
- more explicit, opt-in OpenType features with bounded settings, rather than an
  unrestricted tag surface;
- Arabic harakat inside joining runs, and further joining-script coverage only
  where it needs contextual substitution without visual reordering.

Each candidate must preserve logical cells, copy/search output, cursor columns,
wide-cell boundaries, and fallback behavior. A candidate moves to supported
only with differential tests proving those properties.

## Other deferred extensions

- **Arabic harakat inside joining runs.** Combining marks on an Arabic base
  still break the compatible-run gate so the marked cell uses the mono
  combining path. This is a candidate within the overlay model, not a claim of
  current joining support for marked bases.
- **Open-ended stylistic sets** beyond the explicit `ss01`/`ss02` settings.
  v0.17.0 adds named, bounded legibility controls such as the OpenType `zero`
  feature when the selected font supports them, but an unrestricted `ssXX` or
  raw feature-tag surface stays deferred.

## Sequencing rationale

The shaping-run infrastructure was sequenced before broader ligature coverage
and Arabic joining because both need the same grapheme-cluster and
byte-to-column substrate to anchor overlays correctly. Arabic joining followed
because it is a script-tagged feature application on that same overlay model in
logical cell order.

Sequence-aware width follows the cell-storage work because it changes cluster
ownership without requiring visual reordering. Full complex-script shaping and
BiDi remain outside the overlay model because they require a reversible mapping
between logical terminal cells and a different visual order. SVG-in-OpenType is
independent of that sequence and enters the v0.17.0 work only after its bounded
rasterization and security prerequisites are implemented and tested.
