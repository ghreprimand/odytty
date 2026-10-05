# Unicode 17 northern Indic fixtures

`G1-properties.txt` contains every Unicode 17.0.0 scalar whose Script is
Devanagari, Bengali, Gurmukhi, Gujarati, or Oriya, with General_Category,
Indic_Syllabic_Category, and the final DerivedCoreProperties assignment.
InCB rows carry an additional field. `GraphemeBreakTest-G1.txt` retains every
Unicode GraphemeBreakTest row that starts with a G1 letter and whose other
scalars are in those five scripts, ZWJ, ZWNJ, or U+0308. This is a bounded
corpus, not full UAX #29 conformance. Gurmukhi virama-consonant terminal width
ownership can cross a Unicode extended-grapheme boundary.

The fixtures and generated property tables derive from the Unicode 17.0.0
UCD at <https://www.unicode.org/Public/17.0.0/ucd/>. Input hashes are recorded
in generated headers. The Unicode License v3 is beside this file. Other G1
samples in `tests/indic_width_g1.rs` are project-authored GPL-3.0-only text.

Regenerate with `python3 scripts/unicode-indic-data.py DIRECTORY`, supplying
UnicodeData.txt, Scripts.txt, IndicSyllabicCategory.txt,
DerivedCoreProperties.txt, GraphemeBreakProperty.txt, and GraphemeBreakTest.txt
from that release. The last two files come from the `auxiliary/` directory.
Run `cargo fmt` after generation.
