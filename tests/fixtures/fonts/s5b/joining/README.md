# joining shaping fixtures

Project-authored joining-run samples and OFL subset fonts. References use numeric glyph IDs and UTF-8 byte clusters, in font units, with RTL direction (the scripts' own), und language, cluster level 0 and the OpenType shaper.

Tools: hb-shape (HarfBuzz) 14.5.1; fontTools 4.63.0. Complete copyright and OFL notices are retained in each script-specific OFL file and in OFL.txt. Modified families use OdyTTY fixture names; original copyright and license records remain embedded.

corpus.json in the parent directory records the input scalars and source archive member for every face. Known engine-version differences remain in references with explicit notes.

## Regeneration

Place the original Regular source faces, using their recorded filenames, in a directory named source. Run from the parent s5b directory:

```sh
python3 generate_fixtures.py --sources source --group joining
```

The generator verifies each source hash, runs the exact subsetting commands below, renames the modified family records, and checks source/subset glyph equivalence before writing references. Its name-table rename is part of regeneration. Hinting and glyph names are removed; all layout features remain eligible for closure.

## Syriac-subset.ttf

- Release archive: https://github.com/notofonts/syriac/releases/download/NotoSansSyriac-v3.000/NotoSansSyriac-v3.000.zip
- Archive SHA256: `1d31ac6f3f221332005e10d74af6e0e9bb2fb4ed67c306788d12acba33616538`
- Source member: `NotoSansSyriac/hinted/ttf/NotoSansSyriac-Regular.ttf`
- Source face SHA256: `4440929bf1a47bb50179e8d8495641d208c63d607355e451f29ea2d3e5343290`
- License: Syriac-OFL.txt
- Subset SHA256: `8a8994377f40c90257f3600c83650f0c69312b72d17e839b6d03d24f5a7c0ad2`
- Glyphs: 41; references: 7; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansSyriac-Regular.ttf --unicodes=U+0020,U+0710,U+0712,U+0713,U+0715,U+0720,U+0721,U+072B,U+0730,U+0731 '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=joining/Syriac-subset.ttf
```

## NKo-subset.ttf

- Release archive: https://github.com/notofonts/nko/releases/download/NotoSansNKo-v2.004/NotoSansNKo-v2.004.zip
- Archive SHA256: `8324d3c34a9006a22e121fbe7454ddd0006513a715de027005e894c8a2dafbb2`
- Source member: `NotoSansNKo/hinted/ttf/NotoSansNKo-Regular.ttf`
- Source face SHA256: `c756efb2c40f754107d76fa4e401fc3b8b7edec5cc65db549d3d0236ac6d08a1`
- License: NKo-OFL.txt
- Subset SHA256: `8b4f140ca3757cf821234987525ba6a80deb112eda8a6fc10c16ac780cc05399`
- Glyphs: 29; references: 6; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansNKo-Regular.ttf --unicodes=U+0020,U+07CA,U+07CB,U+07CC,U+07CF,U+07D2,U+07D3,U+07EB,U+07F2,U+07FA '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=joining/NKo-subset.ttf
```

## Mandaic-subset.ttf

- Release archive: https://github.com/notofonts/mandaic/releases/download/NotoSansMandaic-v2.003/NotoSansMandaic-v2.003.zip
- Archive SHA256: `e36cd42da5e7397feee3a3b1046a9d1f0328d29a9fb4c27c716fde630f26a7f4`
- Source member: `NotoSansMandaic/hinted/ttf/NotoSansMandaic-Regular.ttf`
- Source face SHA256: `0ecb2440205b9fe02fce714ddda98bb59892d4f7eacce2c1f12028b172a34de8`
- License: Mandaic-OFL.txt
- Subset SHA256: `81a415abf3f9a3662111faf3ad88f4e56d4e04b804747a123cf227957a956ca8`
- Glyphs: 44; references: 6; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansMandaic-Regular.ttf --unicodes=U+0020,U+0840,U+0841,U+0842,U+0843,U+0844,U+0846,U+0848,U+0859,U+085A '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=joining/Mandaic-subset.ttf
```
