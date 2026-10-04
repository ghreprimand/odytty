# Unicode bidi conformance subsets

These files feed `src/core/bidi/conformance_tests.rs`.

| File | Source | Selection |
| --- | --- | --- |
| `BidiCharacterTest-subset.txt` | Unicode 17.0.0 `ucd/BidiCharacterTest.txt`, SHA-256 `a3e6e905ab5afbe318a96df5401d0372a04cd73ef139ab5e3cf0ae241c255488` | One data line in every 32 per paragraph direction (0 and 1), starting with the first, plus every direction 2 line: 2,894 cases |
| `BidiTest-subset.txt` | Unicode 17.0.0 `ucd/BidiTest.txt`, SHA-256 `888bdfc8090652272d1f859cdb00ae659e2dc6c26740be61ef1d03998a687620` | One data line in every 64, starting with the first, with the `@Levels` and `@Reorder` lines each kept line needs: 12,022 cases across its paragraph directions |

`src/core/bidi/mirrored.rs` is generated from Unicode 17.0.0
`ucd/extracted/DerivedBinaryProperties.txt`, SHA-256
`13dd09d35a9377e33eb388a01e6581d4bfec6b2685316078c341982fa444071a`.

All three are Unicode Character Database content, Copyright Unicode, Inc.,
distributed under the Unicode License v3 in `LICENSE-UNICODE.txt`. The data
rows of the 17.0.0 conformance files are identical to the 16.0.0 files.

Regenerate every file with:

```
python3 scripts/unicode-bidi-data.py <directory holding the three Unicode files>
```

The full files pass in their entirety (91,707 of 91,707 BidiCharacterTest
cases and 770,241 of 770,241 BidiTest cases) through the ignored test
`full_unicode_bidi_corpus`, run with `ODYTTY_UCD_BIDI_DIR` naming the directory
that holds them.
