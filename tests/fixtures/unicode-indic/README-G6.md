# Unicode 17 additional measured-script fixtures

`G6-properties.txt` contains every Unicode 17.0.0 Script scalar in Chakma
(71), Javanese (90), Grantha (85), and Tai Tham (127), including supplement
blocks. It records General_Category, Indic_Syllabic_Category,
Grapheme_Cluster_Break, and DerivedCoreProperties assignments. Source hashes
are in the header; the shared Unicode License v3 is beside this file.
Source: <https://www.unicode.org/Public/17.0.0/ucd/>.

Regenerate using `python3 scripts/unicode-indic-data.py DIRECTORY` with the
six pinned inputs listed in README.md, then `cargo fmt`.
`tests/indic_width_g6.rs` contains project-authored GPL-3.0-only text and
terminal redraw streams. Run `cargo test --locked --test indic_width_g6`.

The terminal width units can cross a grapheme boundary. Chakma U+11134
Pure_Killer is a bounded direct-consonant exception; with intervening ZWJ,
prior separate ownership remains. Javanese and Grantha Mc viramas do not
promote the base without a following consonant. No other Pure_Killer category
or unmeasured script is collapsed. The Unicode 17 GraphemeBreakTest filter
contains zero rows starting with these scripts and containing only the same
script, ZWJ, ZWNJ, or U+0308. These property fixtures do not claim full grapheme
conformance or font-backed shaping.
