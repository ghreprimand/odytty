# Complex-script shaping fixture corpus

The six groups contain 19 minimal OFL Noto faces and project-authored samples for reordering, reph where applicable, conjuncts, stacking, below-base forms, split vowels, and joiner boundaries. Two ordinary consonant/sign controls per face keep basic placement visible. Script-specific constructions and controls are identified by each row's note.

Each group has subset fonts, complete OFL notices, source and subset hashes, exact subsetting commands, and reference.tsv. The reference columns are font, codepoints, glyph_ids, clusters, x_offset, y_offset, x_advance, and note. Glyph arrays use commas; clusters are UTF-8 byte offsets. HarfBuzz and fontTools versions are recorded in every header. Regeneration shapes each source and subset, maps the original glyph IDs through renumbering, and verifies that glyph order, clusters, offsets, and advances agree exactly.

Bengali U+0995 U+09CD U+09B0 remains in northern-indic/reference.tsv with known-diff:harfrust-0.8.4-vs-harfbuzz-14.5. The reference preserves the measured HarfBuzz result; a consumer must explicitly account for that known engine-version difference.

All Unicode text is stored as ASCII U+XXXX sequences. The parent corpus.json defines the inputs, normalization repertoire and project-authored provenance. Shape references describe this corpus and these faces. All samples and generator code are GPL-3.0-only; each modified font remains under its recorded OFL-1.1 license. No full upstream font is distributed here.

Regeneration requires the recorded tool versions and verifies the frozen normalization repertoire. License files remove trailing whitespace while retaining their complete wording and copyright notices; source and normalized license hashes are recorded separately.

Khmer stray-mark references retain the shaper-inserted U+25CC circle; Myanmar omits that mapping and keeps a no-circle control. Only the Myanmar stray-mark source comparison uses a temporary source copy with the circle mapping removed; all ordinary source comparisons use the untouched upstream face.
