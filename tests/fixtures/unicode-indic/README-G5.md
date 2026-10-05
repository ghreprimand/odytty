# Unicode 17 Khmer and Myanmar fixtures

`G5-properties.txt` contains all 146 Khmer and 243 Myanmar Script scalars in
Unicode 17.0.0, with General_Category, Indic_Syllabic_Category,
Grapheme_Cluster_Break, and DerivedCoreProperties assignments. Input hashes
are recorded in the header. The shared Unicode License v3 is beside this file.
Data derives from <https://www.unicode.org/Public/17.0.0/ucd/>.

Regenerate using `python3 scripts/unicode-indic-data.py DIRECTORY` with the
six pinned inputs listed in README.md, then `cargo fmt`.
`tests/indic_width_g5.rs` contains project-authored GPL-3.0-only terminal
streams. Run `cargo test --locked --test indic_width_g5`.

Same-script dependent spacing signs and coeng/invisible-stacker-linked
consonants form bounded source owners. InCB Consonant additionally covers
linked independent vowels in these two scripts. `GraphemeBreakTest-G5.txt`
retains three Khmer and two Myanmar official rows verbatim. Myanmar spacing
sign absorption intentionally crosses the boundary in one row. Khmer U+17A4 and U+17D8 have explicit
one-cell scalar widths. Terminal width ownership does not claim full Unicode
grapheme conformance or font-backed shaping.
