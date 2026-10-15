# Unicode 17 grapheme cluster conformance

`GraphemeBreakTest-17.0.0.txt` is the complete Unicode 17.0.0
`ucd/auxiliary/GraphemeBreakTest.txt` (date line `2025-03-24, 14:45:55 GMT`,
SHA-256 `e2d134d2c52919bace503ebb6a551c1855fe1a1faec18478c78fff254a1793ec`),
copied byte for byte. It holds 766 cases and 1,224 boundary decisions, and
`src/core/grapheme/tests.rs` asserts that the incremental segmenter in
`src/core/grapheme/` reproduces every one of them.

The segmenter's property table, `src/core/grapheme/data.rs`, is generated from
these Unicode 17.0.0 files:

| File | Source | SHA-256 |
| --- | --- | --- |
| `GraphemeBreakProperty.txt` | `ucd/auxiliary/` | `d6b51d1d2ae5c33b451b7ed994b48f1f4dc62b2272a5831e7fd418514a6bae89` |
| `DerivedCoreProperties.txt` (InCB) | `ucd/` | `24c7fed1195c482faaefd5c1e7eb821c5ee1fb6de07ecdbaa64b56a99da22c08` |
| `emoji-data.txt` (Extended_Pictographic) | `ucd/emoji/` | `2cb2bb9455cda83e8481541ecf5b6dfda66a3bb89efa3fa7c5297eccf607b72b` |

All of them are Unicode Character Database content, Copyright Unicode, Inc.,
distributed under the Unicode License v3 in `LICENSE-UNICODE.txt` beside this
file. Source: <https://www.unicode.org/Public/17.0.0/ucd/>.

Regenerate the table and this fixture with:

```
python3 scripts/unicode-grapheme-data.py <directory holding the four files>
cargo fmt
```

The generator refuses any input whose SHA-256 differs from the pins above, so
a later Unicode version cannot be picked up silently.

Grapheme boundaries are not terminal cell ownership. The segmenter decides
where Unicode places cluster boundaries; how scalars become cells and how wide
each cell owner is remain separate, bounded terminal rules.
