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
advances or cell counts. That covers ASCII contextual ligatures and static
color glyphs correctly. It does not cover scripts whose correct rendering
requires reordering or reshaping across cell boundaries (see Deferred, below).

## What v0.11.0 added

- **Shaping-run infrastructure.** The presentation shaper now groups cells
  into shaping runs by grapheme cluster, with a byte-to-column map that anchors
  each shaped glyph back to the source cell(s) it came from, and compatible-run
  boundary detection. Runs break at wide continuations, hidden cells,
  color-glyph coverage, cells carrying combining marks, and any bold/italic
  face change, so those categories never merge into a shaped span. Live
  overlay eligibility is unchanged -- ASCII-graphic bases only -- so default
  rendering stays byte-identical to the pre-infrastructure path, pinned by a
  differential test. This is groundwork: the grapheme and byte-to-column
  plumbing is the shared substrate later curated non-ASCII ligature coverage
  will build on, not a behavior change by itself.
- **Static color glyphs (COLR/CPAL v0).** The color-glyph path renders static
  COLR/CPAL v0 layer compositions in addition to the existing bitmap-strike
  formats, including stock Windows Segoe UI Emoji, which previously fell back
  to the monochrome path. See `docs/features.md` for the full color-emoji
  support statement.
- **Extended ligature coverage beyond ASCII.** In progress; not landed as of
  this writing. The shaping-run infrastructure above is the prerequisite for
  it. Track its landing status in `TODO.md`.

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
- **SVG-in-OpenType.** Deferred; see `docs/features.md` for the current
  color-glyph format support statement.
- **COLR v1 Paint graphs.** Deferred alongside SVG-in-OT; a face or glyph that
  exposes only these formats falls back to the monochrome path.

## Sequencing rationale

The shaping-run infrastructure was sequenced before broader ligature coverage
because both extended ligatures and color-glyph-aware runs need the same
grapheme-cluster and byte-to-column substrate to anchor overlays correctly;
building it once and reusing it avoids two independent, potentially divergent
run-boundary implementations. Complex-script shaping and bidi are sequenced
after because they are model-level questions rather than extensions of the
current overlay approach, and deserve their own design pass rather than being
folded into the anchored-span model as an afterthought.
