# Unicode 17 combining-mark fixture

`marks.txt` lists every Unicode 17.0.0 range whose General_Category is Mn
(nonspacing mark) or Me (enclosing mark). The glyph atlas table
`src/atlas/marks.rs` is generated from the same input: those ranges less the
variation selectors U+FE00..U+FE0F and U+E0100..U+E01EF, which the atlas
classifies separately. The atlas draws a listed mark over its base cell and
keeps it off the fallback-font chain.

The input is `extracted/DerivedGeneralCategory.txt` from
<https://www.unicode.org/Public/17.0.0/ucd/>; its SHA-256 is pinned in the
generator and recorded in both generated headers. Data is licensed by the
adjacent Unicode License v3.

Regenerate with `python3 scripts/unicode-mark-data.py DIRECTORY`, where the
directory holds that file, then `cargo fmt`. Run
`cargo test --locked --lib atlas::tests::marks`, which compares the table with
this fixture for every scalar value.
