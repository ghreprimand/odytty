# Unicode 17 southern Indic fixtures

`G2-properties.txt` contains every scalar whose Unicode 17.0.0 Script is
Tamil, Telugu, Kannada, or Malayalam. It records General_Category,
Indic_Syllabic_Category, Grapheme_Cluster_Break, and the final assignment in
DerivedCoreProperties. InCB assignments occupy two fields. Input SHA-256
hashes appear in the header. Source: <https://www.unicode.org/Public/17.0.0/ucd/>;
GraphemeBreakProperty.txt comes from auxiliary/. Unicode License v3 is in
LICENSE-UNICODE.txt beside this file.

`tests/indic_width_g2.rs` contains project-authored GPL-3.0-only minimal text
and terminal redraw streams. The eight frozen-width examples independently
combine Unicode consonants and signs, and their cursor-width failures were
confirmed in the pinned ucs-detect 2.3.8 / wcwidth 0.9.1 baseline. No UDHR prose
or packaged language corpus text is copied into these fixtures.

Terminal width ownership is distinct from Unicode extended-grapheme boundaries.
Tamil virama-consonant ownership can cross an extended-grapheme boundary.
The Unicode 17 GraphemeBreakTest file has no rows starting with a G2 scalar
whose remaining scalars are G2, ZWJ, ZWNJ, or U+0308. These property-derived
regressions are bounded terminal-ownership tests, not a full UAX #29 pass.
All G2 ownership and width assertions run with
`cargo test --locked --test indic_width_g2`. Regenerate the property subset and
production tables with `python3 scripts/unicode-indic-data.py` using the same
Unicode 17 input directory described in README.md beside these fixtures.
