# Text Shaping Roadmap

Companion to the shaping/ligature summary in `docs/features.md`. This document
states the current shaping model, what v0.11.0 changed, and what remains
deferred, with the reasoning behind the sequencing. It does not restate the
user-facing feature description; see `docs/features.md` for that.

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
requires reordering or reshaping across cell boundaries (see Deferred, below).

## What v0.11.0 added

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
  to the monochrome path. See `docs/features.md` for the full color-emoji
  support statement.
- **COLR v1 Paint graphs.** The same color-glyph atlas now accepts v1-only
  glyphs through Fontations' guarded graph traversal. Solid fills, gradients,
  transforms, clips, and composites rasterize into premultiplied RGBA after
  bitmap and v0 sources decline the glyph, preserving both established paths.
- **Extended ligature coverage beyond ASCII.** Landed as the curated allowlist
  above — not an open feature-tag surface.
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

## What remains deferred, and why

- **Full complex-script shaping** (Brahmic and related scripts that require
  glyph reordering, cluster reshaping, or reassembly across what OdyTTY treats
  as separate grid cells). This is not a missing feature so much as a model
  conflict: reordering scripts need shaping to change which glyph occupies
  which visual position relative to the source text, which the anchored
  overlay-on-a-fixed-grid approach is built specifically not to do. Supporting
  it correctly means either abandoning the one-character-per-cell model for
  those scripts or a substantially different reshaping design than the overlay
  spans described above. Stated plainly: OdyTTY does not currently render
  these scripts with correct shaping, and there is no partial or approximate
  claim being made in their place.
- **Bidirectional text.** No bidi reordering is implemented. Right-to-left
  input is stored and drawn in logical cell order without visual reordering.
  Arabic joining forms are shaped within that logical order (see above); that
  does **not** mean RTL runs are laid out in visual reading order - cells
  remain left-to-right exactly as stored.
- **Arabic harakat inside joining runs.** Combining marks on an Arabic base
  still break the compatible-run gate so the marked cell uses the mono
  combining path. Base-letter joining without harakat is what this release
  delivers.
- **Open-ended stylistic sets** beyond the explicit `ss01`/`ss02` settings.
  Additional `ssXX` tags stay deferred.
- **SVG-in-OpenType.** Deferred; see `docs/features.md` for the current
  color-glyph format support statement.

## Sequencing rationale

The shaping-run infrastructure was sequenced before broader ligature coverage
and Arabic joining because both need the same grapheme-cluster and
byte-to-column substrate to anchor overlays correctly; building it once and
reusing it avoids two independent, potentially divergent run-boundary
implementations. Arabic joining was sequenced next because it is a
script-tagged feature application on that same overlay model (logical LTR
cells, no reordering). Full complex-script shaping and bidi remain sequenced
after because they are model-level questions rather than extensions of the
current overlay approach, and deserve their own design pass rather than being
folded into the anchored-span model as an afterthought.
