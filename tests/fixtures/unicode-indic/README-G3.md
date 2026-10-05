# Unicode 17 Sinhala fixtures

`G3-properties.txt` contains every scalar whose Unicode 17.0.0 Script is
Sinhala, including the archaic-number block. It records General_Category,
Indic_Syllabic_Category, Grapheme_Cluster_Break, and the final assignment in
DerivedCoreProperties. InCB rows have an additional field. Input SHA-256
hashes appear in the header. The shared Unicode License v3 is in
LICENSE-UNICODE.txt beside this file.

The data derives from <https://www.unicode.org/Public/17.0.0/ucd/>.
Regenerate with `python3 scripts/unicode-indic-data.py DIRECTORY`, using the
Unicode 17 inputs listed in README.md beside these fixtures, then `cargo fmt`.
`tests/indic_width_g3.rs` uses project-authored GPL-3.0-only text and terminal
redraw streams. Run it with `cargo test --locked --test indic_width_g3`.

Sinhala virama-consonant terminal width ownership can cross an extended-grapheme
boundary. The Unicode 17 GraphemeBreakTest filter has no rows starting with
a Sinhala scalar whose other scalars are Sinhala, ZWJ, ZWNJ, or U+0308.
These property-derived fixtures test bounded terminal ownership;
they do not claim full Unicode grapheme conformance or font-backed shaping.
