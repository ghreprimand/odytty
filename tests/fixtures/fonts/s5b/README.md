# Complex-script shaping fixture corpus

The twelve groups contain 42 minimal OFL Noto faces and project-authored samples for reordering, reph where applicable, conjuncts, stacking, below-base forms, split vowels, and joiner boundaries; the joining groups (Syriac, N'Ko, Mandaic, Adlam, and Hanifi Rohingya in joining; horizontal Mongolian in joining-ltr) hold whole contextual joining runs with their transparent marks instead of single owners. Two ordinary consonant/sign controls per face keep basic placement visible. Script-specific constructions and controls are identified by each row's note.

Each group has subset fonts, complete OFL notices, source and subset hashes, exact subsetting commands, and reference.tsv. The reference columns are font, codepoints, glyph_ids, clusters, x_offset, y_offset, x_advance, and note. Glyph arrays use commas; clusters are UTF-8 byte offsets. HarfBuzz and fontTools versions are recorded in every header, with the shaping direction: left to right for every group except rtl (Hebrew and Kharoshthi) and joining, which are shaped right to left as production shapes them. Regeneration shapes each source and subset, maps the original glyph IDs through renumbering, and verifies that glyph order, clusters, offsets, and advances agree exactly.

Bengali U+0995 U+09CD U+09B0 (below-base ra) is in northern-indic/reference.tsv like every other row. `harfrust` 0.8.4 shaped it differently from HarfBuzz; 0.14.0 matches, so no row carries a known-diff note. A row that the shaper cannot match is marked `known-diff:` in its note and asserted by the consumer, never skipped.

All Unicode text is stored as ASCII U+XXXX sequences. The parent corpus.json defines the inputs, normalization repertoire and project-authored provenance. Shape references describe this corpus and these faces. All samples and generator code are GPL-3.0-only; each modified font remains under its recorded OFL-1.1 license. No full upstream font is distributed here.

Regeneration requires the recorded tool versions and verifies the frozen normalization repertoire. License files remove trailing whitespace while retaining their complete wording and copyright notices; source and normalized license hashes are recorded separately.

Khmer stray-mark references retain the shaper-inserted U+25CC circle; Myanmar omits that mapping and keeps a no-circle control. Only the Myanmar stray-mark source comparison uses a temporary source copy with the circle mapping removed; all ordinary source comparisons use the untouched upstream face.

`collections/TamilSangamMN-layout.ttc` packs the committed Tamil and Grantha subsets into a four-face collection in the face order of the macOS Tamil Sangam MN collection, for the script-face fallback tests. It is built by `generate_collection_fixture.py` from the subsets alone; see `collections/README.md`.
