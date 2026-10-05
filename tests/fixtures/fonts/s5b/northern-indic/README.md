# northern-indic shaping fixtures

Project-authored owner samples and OFL subset fonts. References use numeric glyph IDs and UTF-8 byte clusters, in font units, with LTR direction, und language, cluster level 0 and the OpenType shaper.

Tools: hb-shape (HarfBuzz) 14.5.1; fontTools 4.63.0. Complete copyright and OFL notices are retained in each script-specific OFL file and in OFL.txt. Modified families use OdyTTY fixture names; original copyright and license records remain embedded.

corpus.json in the parent directory records the input scalars and source archive member for every face. Its explicit repertoire includes decomposition characters required for splitting vowels and SARA AM. Default-ignorable joiners remain in the input corpus. Known engine-version differences remain in references with explicit notes.

## Regeneration

Place the original Regular source faces, using their recorded filenames, in a directory named source. Run from the parent s5b directory:

```sh
python3 generate_fixtures.py --sources source --group northern-indic
```

The generator verifies each source hash, runs the exact subsetting commands below, renames the modified family records, and checks source/subset glyph equivalence before writing references. Its name-table rename is part of regeneration. Hinting and glyph names are removed; all layout features remain eligible for closure.

## Devanagari-subset.ttf

- Release archive: https://github.com/notofonts/devanagari/releases/download/NotoSansDevanagari-v2.007/NotoSansDevanagari-v2.007.zip
- Archive SHA256: `820c7da45b1e63562cb41c0a8cac5d9a4202312043a3a040ed1325857ef469b1`
- Source member: `NotoSansDevanagari/hinted/ttf/NotoSansDevanagari-Regular.ttf`
- Source face SHA256: `4e3c66638958c3e2ab5d37f47a8deb89fffeb7be9985c665a519bbc7ba762313`
- License: Devanagari-OFL.txt
- Subset SHA256: `e32080e7289298be768d20cc31bb44b83fe16b937bddc653aeba3eda16d511a7`
- Glyphs: 111; references: 13; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansDevanagari-Regular.ttf --unicodes=U+0020,U+0902,U+0915,U+0926,U+0927,U+0930,U+0936,U+0937,U+0939,U+093E,U+093F,U+0943,U+0947,U+094D,U+200C,U+200D '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=northern-indic/Devanagari-subset.ttf
```

## Bengali-subset.ttf

- Release archive: https://github.com/notofonts/bengali/releases/download/NotoSansBengali-v3.011/NotoSansBengali-v3.011.zip
- Archive SHA256: `2aca24bf71665e66c32e14862ee58857dd8b8b07b6321ec7c96f3121ad6683b1`
- Source member: `NotoSansBengali/hinted/ttf/NotoSansBengali-Regular.ttf`
- Source face SHA256: `b55c62ee531e3214da6c0701daecea89a52ba42db7d8206b92e6b51f397a3193`
- License: Bengali-OFL.txt
- Subset SHA256: `2a3bc03d9c5fd389c543c03e1b042a50781df9001c4d20d56a776c86f30bb1f7`
- Glyphs: 53; references: 9; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansBengali-Regular.ttf --unicodes=U+0020,U+0995,U+09B0,U+09B7,U+09BE,U+09BF,U+09C7,U+09CB,U+09CD,U+200C,U+200D '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=northern-indic/Bengali-subset.ttf
```

## Gurmukhi-subset.ttf

- Release archive: https://github.com/notofonts/gurmukhi/releases/download/NotoSansGurmukhi-v2.004/NotoSansGurmukhi-v2.004.zip
- Archive SHA256: `a55ee5ee831ce3a4bb73bdc46bd0a02db6eb9445c370a0afbc529379ed8cea52`
- Source member: `NotoSansGurmukhi/hinted/ttf/NotoSansGurmukhi-Regular.ttf`
- Source face SHA256: `658d0207da305a1411c539a8b0bbeda64d4146e54fb4827facddb890b6b90d74`
- License: Gurmukhi-OFL.txt
- Subset SHA256: `fc4eb5d62de69c5fc041404776d16c7617f349d5083db49ffe600bf5be2ec192`
- Glyphs: 19; references: 9; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansGurmukhi-Regular.ttf --unicodes=U+0020,U+0A15,U+0A30,U+0A39,U+0A3E,U+0A3F,U+0A40,U+0A4B,U+0A4D,U+200C,U+200D '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=northern-indic/Gurmukhi-subset.ttf
```

## Gujarati-subset.ttf

- Release archive: https://github.com/notofonts/gujarati/releases/download/NotoSansGujarati-v2.106/NotoSansGujarati-v2.106.zip
- Archive SHA256: `a4ff1dd03a4998ba08ee7ee43be2aca7594a884982049e48c25ccc48991474ed`
- Source member: `NotoSansGujarati/hinted/ttf/NotoSansGujarati-Regular.ttf`
- Source face SHA256: `9b5a7aaeeb649a2e75a49d8b006a1f87db1b61c0df3b001609f4e0725d88dbf6`
- License: Gujarati-OFL.txt
- Subset SHA256: `7082ed5c1b7795251c5b1d9b06117ddb3de6232ddc46b7a56840fe8c34260fdd`
- Glyphs: 44; references: 9; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansGujarati-Regular.ttf --unicodes=U+0020,U+0A95,U+0AB0,U+0AB7,U+0ABE,U+0ABF,U+0AC7,U+0ACB,U+0ACD,U+200C,U+200D '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=northern-indic/Gujarati-subset.ttf
```

## Odia-subset.ttf

- Release archive: https://github.com/notofonts/oriya/releases/download/NotoSansOriya-v2.007/NotoSansOriya-v2.007.zip
- Archive SHA256: `025b637e2d391f422ae12f4eb365876ce3b195283025ababb74ecf6baae35442`
- Source member: `NotoSansOriya/hinted/ttf/NotoSansOriya-Regular.ttf`
- Source face SHA256: `a16645d056017927406546aa78e4ce15e782fd8783467267b75450453d007415`
- License: Odia-OFL.txt
- Subset SHA256: `12b294e54a5dbfb797f11024bfb35d1b98c393ac0b32e54bdc81a3291919a703`
- Glyphs: 63; references: 9; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansOriya-Regular.ttf --unicodes=U+0020,U+0B15,U+0B30,U+0B37,U+0B3E,U+0B3F,U+0B47,U+0B4B,U+0B4D,U+200C,U+200D '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=northern-indic/Odia-subset.ttf
```
