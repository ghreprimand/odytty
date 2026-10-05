# g6 shaping fixtures

Project-authored owner samples and OFL subset fonts. References use numeric glyph IDs and UTF-8 byte clusters, in font units, with LTR direction, und language, cluster level 0 and the OpenType shaper.

Tools: hb-shape (HarfBuzz) 14.5.1; fontTools 4.63.0. Complete copyright and OFL notices are retained in each script-specific OFL file and in OFL.txt. Modified families use OdyTTY fixture names; original copyright and license records remain embedded.

corpus.json in the parent directory records the input scalars and source archive member for every face. Its explicit repertoire includes decomposition characters required for splitting vowels and SARA AM. Default-ignorable joiners remain in the input corpus. Known engine-version differences remain in references with explicit notes.

## Regeneration

Place the original Regular source faces, using their recorded filenames, in a directory named source. Run from the parent s5b directory:

```sh
python3 generate_fixtures.py --sources source --group g6
```

The generator verifies each source hash, runs the exact subsetting commands below, renames the modified family records, and checks source/subset glyph equivalence before writing references. Its name-table rename is part of regeneration. Hinting and glyph names are removed; all layout features remain eligible for closure.

## Chakma-subset.ttf

- Release archive: https://github.com/notofonts/chakma/releases/download/NotoSansChakma-v2.003/NotoSansChakma-v2.003.zip
- Archive SHA256: `18bcea554ee3457ccf9c26cd6aa498392b792510abc3a7e8405d31e24e003e1b`
- Source member: `NotoSansChakma/hinted/ttf/NotoSansChakma-Regular.ttf`
- Source face SHA256: `60ce1d029e35b432dd68cc9f6c94f69bd84d8c97f28f06130186606dd2c3325d`
- License: Chakma-OFL.txt
- Subset SHA256: `9ff135a1d392c7eb91792b97673960ba3bac3b7b3acbd426c3713734fafc9ce5`
- Glyphs: 15; references: 6; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansChakma-Regular.ttf --unicodes=U+0020,U+200C,U+200D,U+11107,U+11127,U+11128,U+1112C,U+11134 '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=g6/Chakma-subset.ttf
```

## Javanese-subset.ttf

- Release archive: https://github.com/notofonts/javanese/releases/download/NotoSansJavanese-v2.005/NotoSansJavanese-v2.005.zip
- Archive SHA256: `3d096aeee4dc607a91e7568785595643cb5fde4a0b7c9c7f13a762c48d37a0cf`
- Source member: `NotoSansJavanese/hinted/ttf/NotoSansJavanese-Regular.ttf`
- Source face SHA256: `81fdea70d379989bafea65eae5a6a96144991b437415744716a49a56f09f747a`
- License: Javanese-OFL.txt
- Subset SHA256: `604fca81402b4033d98b0ff2c2c95c5f06e758f77dd0930ffaa9c23ba6e35720`
- Glyphs: 17; references: 8; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansJavanese-Regular.ttf --unicodes=U+0020,U+200C,U+200D,U+A98F,U+A9B4,U+A9B6,U+A9BA,U+A9BF,U+A9C0 '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=g6/Javanese-subset.ttf
```

## Grantha-subset.ttf

- Release archive: https://github.com/notofonts/grantha/releases/download/NotoSansGrantha-v2.005/NotoSansGrantha-v2.005.zip
- Archive SHA256: `a66a6cb8acd222363ea980308360fb6b30d29c27f147e4d664316c34fd08549f`
- Source member: `NotoSansGrantha/hinted/ttf/NotoSansGrantha-Regular.ttf`
- Source face SHA256: `41dd39f2f16e9539751c732f8d276079b98277bec2ea5945232a7ac594198000`
- License: Grantha-OFL.txt
- Subset SHA256: `f655a0fb348ebf094b87b5f407c3ab8b6198478ea8337ec63032d240c2820285`
- Glyphs: 39; references: 6; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansGrantha-Regular.ttf --unicodes=U+0020,U+200C,U+200D,U+11315,U+11337,U+1133E,U+11347,U+1134B,U+1134D '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=g6/Grantha-subset.ttf
```

## TaiTham-subset.ttf

- Release archive: https://github.com/notofonts/tai-tham/releases/download/NotoSansTaiTham-v2.002/NotoSansTaiTham-v2.002.zip
- Archive SHA256: `12eb024bab33c9b8deeb823cf96107219719ed519f62cbe3f58295722113baea`
- Source member: `NotoSansTaiTham/hinted/ttf/NotoSansTaiTham-Regular.ttf`
- Source face SHA256: `b94134811a2f8c26631a728837bb72f74dad87402810fc46b34c310312a6b369`
- License: TaiTham-OFL.txt
- Subset SHA256: `ec10227324287687d492fe99223d029c5386ab0cb694c6d7782a156d1d8c73c7`
- Glyphs: 17; references: 7; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansTaiTham-Regular.ttf --unicodes=U+0020,U+1A20,U+1A55,U+1A60,U+1A63,U+1A65,U+1A6E,U+200C,U+200D '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=g6/TaiTham-subset.ttf
```
