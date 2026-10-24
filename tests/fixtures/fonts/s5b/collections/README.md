# Collection fixture

`TamilSangamMN-layout.ttc` models the face order of the macOS system file
`Tamil Sangam MN.ttc`, where Grantha Sangam MN is not a file of its own but a
face inside the Tamil collection:

| Face | macOS face | Fixture face |
|---|---|---|
| 0 | Tamil Sangam MN | `../southern-indic/Tamil-subset.ttf` |
| 1 | Grantha Sangam MN | `../g6/Grantha-subset.ttf` |
| 2 | Tamil Sangam MN Bold | `../southern-indic/Tamil-subset.ttf` |
| 3 | Grantha Sangam MN Bold | `../g6/Grantha-subset.ttf` |

It contains no data from a macOS font. Its faces are the committed OFL subsets
named above, unchanged except for their table-record offsets; their notices are
retained in [`Tamil-OFL.txt`](Tamil-OFL.txt) and
[`Grantha-OFL.txt`](Grantha-OFL.txt), and their provenance is recorded in the
`southern-indic` and `g6` READMEs. The script-face fallback tests in
`src/text/script_fallback_tests.rs` use it.

## Regeneration

Run from the parent `s5b` directory with Python 3; only the standard library is
needed, and the output is byte-for-byte reproducible:

```sh
python3 generate_collection_fixture.py
```

- Collection SHA256: `47248d58f392fae5bc779003f8a06d32219b7aa853082d22e8a7ce61b24d30e5`
