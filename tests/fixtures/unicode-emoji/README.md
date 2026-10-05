# Unicode 17 emoji width fixtures

`G7-sequences.txt` contains 371 emoji variation sequences, 665 modifier
sequences, 12 VS16 keycaps, 259 RGI flags, and 1,614 RGI ZWJ sequences.
The four Unicode input SHA-256 values are pinned in the generator and fixture
header. Data is licensed by the adjacent Unicode License v3.
Sources are <https://www.unicode.org/Public/17.0.0/ucd/emoji/> and
<https://www.unicode.org/Public/17.0.0/emoji/>.

Regenerate with `python3 scripts/unicode-emoji-data.py DIRECTORY`, using
emoji-data.txt, emoji-variation-sequences.txt, emoji-sequences.txt, and
emoji-zwj-sequences.txt, then `cargo fmt`. Generated ZWJ tables are split to
stay below the production-file limit.

Run `cargo test --locked --test emoji_width_g7`. The authored terminal
streams use GPL-3.0-only project text. Unicode 17 listed VS16 bases promote
to two cells; modifier bases with one skin-tone modifier, any pair of regional
indicators, and listed fully qualified RGI ZWJ sequences use one two-cell
owner. Non-RGI joins retain their prior separate owners. VS15 does not demote,
standalone regional indicators stay one cell, and keycaps without VS16 stay
one cell. Those compatibility choices are not generic wcwidth conformance.
