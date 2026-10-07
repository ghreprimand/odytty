# Unicode 17 legacy-computing block names

`names.txt` holds the code point and character name of the 60 sextants
(U+1FB00..U+1FB3B), 230 octants (U+1CD00..U+1CDE5), 8 triangular blocks
(U+1FB68..U+1FB6F), and 10 upper and right ladder blocks (U+1FB82..U+1FB8B),
taken from the Unicode Character Database. The UnicodeData.txt SHA-256 is
pinned in the generator and the fixture header. Data is licensed by the
adjacent Unicode License v3. Source is
<https://www.unicode.org/Public/17.0.0/ucd/UnicodeData.txt>.

Regenerate with `python3 scripts/unicode-legacy-names.py UnicodeData.txt`,
redirecting standard output to `names.txt`.

Run `cargo test --locked --test legacy_computing_names`. The test derives
every expected filled region from the names alone: sextant and octant digits
number the cells row by row, a triangular block is the quarter between the
two cell diagonals on each named edge, and a ladder block covers the named
number of eighths. None of the expectations come from OdyTTY's renderer.
