# brahmic-a shaping fixtures

Project-authored owner samples and OFL subset fonts. References use numeric glyph IDs and UTF-8 byte clusters, in font units, with LTR direction, und language, cluster level 0 and the OpenType shaper.

Tools: hb-shape (HarfBuzz) 14.5.1; fontTools 4.63.0. Complete copyright and OFL notices are retained in each script-specific OFL file and in OFL.txt. Modified families use OdyTTY fixture names; original copyright and license records remain embedded.

corpus.json in the parent directory records the input scalars and source archive member for every face. Known engine-version differences remain in references with explicit notes.

## Regeneration

Place the original Regular source faces, using their recorded filenames, in a directory named source. Run from the parent s5b directory:

```sh
python3 generate_fixtures.py --sources source --group brahmic-a
```

The generator verifies each source hash, runs the exact subsetting commands below, renames the modified family records, and checks source/subset glyph equivalence before writing references. Its name-table rename is part of regeneration. Hinting and glyph names are removed; all layout features remain eligible for closure.

## Balinese-subset.ttf

- Release archive: https://github.com/notofonts/balinese/releases/download/NotoSansBalinese-v2.006/NotoSansBalinese-v2.006.zip
- Archive SHA256: `b3fef31d0a12e2675f802694d37eacae0d141598c514f5d45b4681c4997d52be`
- Source member: `NotoSansBalinese/hinted/ttf/NotoSansBalinese-Regular.ttf`
- Source face SHA256: `cee58a184e7e00c4332087ac01870a69ac52fd5fa17ea3783ac728c945af1827`
- License: Balinese-OFL.txt
- Subset SHA256: `21631560318a63de5cafa118c04003ea5ea087d4ecc63a163364845d23925e15`
- Glyphs: 29; references: 10; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansBalinese-Regular.ttf --unicodes=U+0020,U+1B03,U+1B13,U+1B22,U+1B2D,U+1B35,U+1B36,U+1B38,U+1B3E,U+1B40,U+1B44,U+200C,U+200D '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=brahmic-a/Balinese-subset.ttf
```

## Sundanese-subset.ttf

- Release archive: https://github.com/notofonts/sundanese/releases/download/NotoSansSundanese-v2.005/NotoSansSundanese-v2.005.zip
- Archive SHA256: `79155cf623759415986c6f86e96a92264fc0621f583896ee569f184700c46b9d`
- Source member: `NotoSansSundanese/hinted/ttf/NotoSansSundanese-Regular.ttf`
- Source face SHA256: `6cf8ae0d063b8af8ab1bc1b8c7041ac92104e74989dcc1217763723ca6a50c9e`
- License: Sundanese-OFL.txt
- Subset SHA256: `a31ff168f277c859195759ce5cdc172d40778376abaf69953f18fc6314b90639`
- Glyphs: 12; references: 9; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansSundanese-Regular.ttf --unicodes=U+0020,U+1B8A,U+1B93,U+1BA1,U+1BA2,U+1BA4,U+1BA6,U+1BAA,U+1BAB,U+200C,U+200D '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=brahmic-a/Sundanese-subset.ttf
```

## Batak-subset.ttf

- Release archive: https://github.com/notofonts/batak/releases/download/NotoSansBatak-v2.005/NotoSansBatak-v2.005.zip
- Archive SHA256: `035ac2898a02562b5a34886351935a9a0d9a61d1acb4b60248b42fe931f62bcb`
- Source member: `NotoSansBatak/hinted/ttf/NotoSansBatak-Regular.ttf`
- Source face SHA256: `e92e4bffffc64f7265ca9da1097e8b8be31fb963b8148edd06d1a22e1a449160`
- License: Batak-OFL.txt
- Subset SHA256: `c6b2a2ac0f926b055c2365e067890b27a082cd8ac8118de5d281d9adffead95d`
- Glyphs: 12; references: 6; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansBatak-Regular.ttf --unicodes=U+0020,U+1BC5,U+1BC7,U+1BE7,U+1BEA,U+1BEE,U+1BF0,U+1BF2 '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=brahmic-a/Batak-subset.ttf
```

## Buginese-subset.ttf

- Release archive: https://github.com/notofonts/buginese/releases/download/NotoSansBuginese-v2.002/NotoSansBuginese-v2.002.zip
- Archive SHA256: `7b1f8fcd6f0959b791b938ff40bf414b082ca3f40b362d20a1ba14486a759ca3`
- Source member: `NotoSansBuginese/hinted/ttf/NotoSansBuginese-Regular.ttf`
- Source face SHA256: `bc19f17d7f6e8f280c2cc95ef6d1b67fac25becfe98722f482039a4d84f3c9ba`
- License: Buginese-OFL.txt
- Subset SHA256: `bece48147539480312d343844827e8b2d19c39ce89110bf5fc98221e0ccc714e`
- Glyphs: 8; references: 6; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansBuginese-Regular.ttf --unicodes=U+0020,U+1A00,U+1A01,U+1A17,U+1A18,U+1A19,U+1A1A '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=brahmic-a/Buginese-subset.ttf
```

## Cham-subset.ttf

- Release archive: https://github.com/notofonts/cham/releases/download/NotoSansCham-v2.005/NotoSansCham-v2.005.zip
- Archive SHA256: `24d8125d3110066e833704d04c5aae815c86f57bae433e4d6a1e57de7bc966c0`
- Source member: `NotoSansCham/hinted/ttf/NotoSansCham-Regular.ttf`
- Source face SHA256: `d48a34e02b4096eb3f2e008b74459789540598c2274d65fc53012517d0b08d92`
- License: Cham-OFL.txt
- Subset SHA256: `b74e0353f5cde3094309336d88dd8260bbf85b5af2f7447f19e48aa3d2b15330`
- Glyphs: 10; references: 6; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansCham-Regular.ttf --unicodes=U+0020,U+AA06,U+AA0A,U+AA2A,U+AA2F,U+AA33,U+AA34,U+AA43 '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=brahmic-a/Cham-subset.ttf
```

## Lepcha-subset.ttf

- Release archive: https://github.com/notofonts/lepcha/releases/download/NotoSansLepcha-v2.006/NotoSansLepcha-v2.006.zip
- Archive SHA256: `26f9e6d45f3740599d366d490909c49d5a30f86ea957febf3dd36ce63b143e24`
- Source member: `NotoSansLepcha/hinted/ttf/NotoSansLepcha-Regular.ttf`
- Source face SHA256: `8dad9629d709f0dba5d769988d1466daf300751f6bba2659465aaa6214261590`
- License: Lepcha-OFL.txt
- Subset SHA256: `f943662c67857fa58fc9921772cd2bd37d556ec8fb79383ef12f3e427f9eb90a`
- Glyphs: 17; references: 6; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansLepcha-Regular.ttf --unicodes=U+0020,U+1C00,U+1C0E,U+1C24,U+1C25,U+1C26,U+1C27,U+1C2C,U+1C2D '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=brahmic-a/Lepcha-subset.ttf
```

## Limbu-subset.ttf

- Release archive: https://github.com/notofonts/limbu/releases/download/NotoSansLimbu-v2.005/NotoSansLimbu-v2.005.zip
- Archive SHA256: `6cdd3712a06017e40bd1069d8f271932d03bd85aa6d8fa7cb361b32d747d1e94`
- Source member: `NotoSansLimbu/hinted/ttf/NotoSansLimbu-Regular.ttf`
- Source face SHA256: `5cea5d0f2a089c36ea24e8481f65a2aff5d4b0890db440326cea2c808f82d582`
- License: Limbu-OFL.txt
- Subset SHA256: `8866b1f9aa015de43c7584751c9b228a0df05c2c2e03ed1ce29704c0cc410034`
- Glyphs: 11; references: 6; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansLimbu-Regular.ttf --unicodes=U+0020,U+1901,U+1915,U+1921,U+1922,U+1923,U+1929,U+192A,U+1930 '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=brahmic-a/Limbu-subset.ttf
```
