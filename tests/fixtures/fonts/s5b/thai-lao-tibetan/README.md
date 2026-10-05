# thai-lao-tibetan shaping fixtures

Project-authored owner samples and OFL subset fonts. References use numeric glyph IDs and UTF-8 byte clusters, in font units, with LTR direction, und language, cluster level 0 and the OpenType shaper.

Tools: hb-shape (HarfBuzz) 14.5.1; fontTools 4.63.0. Complete copyright and OFL notices are retained in each script-specific OFL file and in OFL.txt. Modified families use OdyTTY fixture names; original copyright and license records remain embedded.

corpus.json in the parent directory records the input scalars and source archive member for every face. Its explicit repertoire includes decomposition characters required for splitting vowels and SARA AM. Default-ignorable joiners remain in the input corpus. Known engine-version differences remain in references with explicit notes.

## Regeneration

Place the original Regular source faces, using their recorded filenames, in a directory named source. Run from the parent s5b directory:

```sh
python3 generate_fixtures.py --sources source --group thai-lao-tibetan
```

The generator verifies each source hash, runs the exact subsetting commands below, renames the modified family records, and checks source/subset glyph equivalence before writing references. Its name-table rename is part of regeneration. Hinting and glyph names are removed; all layout features remain eligible for closure.

## Thai-subset.ttf

- Release archive: https://github.com/notofonts/thai/releases/download/NotoSansThai-v2.002/NotoSansThai-v2.002.zip
- Archive SHA256: `af889cc673fc714060ce5e4e088fbad32aa4c0571a19958efeaff128a22da485`
- Source member: `NotoSansThai/hinted/ttf/NotoSansThai-Regular.ttf`
- Source face SHA256: `61cf814eec46b294d6ea4401ac295d0cecd5207bd2331dcc5a15e7301d30ee44`
- License: Thai-OFL.txt
- Subset SHA256: `fe1882c1546492d6f69ab806fead8d4f383b6891a3b54b9b00381a270540b9d4`
- Glyphs: 24; references: 8; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansThai-Regular.ttf --unicodes=U+0020,U+0E01,U+0E0D,U+0E1B,U+0E30,U+0E32,U+0E33,U+0E34,U+0E38,U+0E48,U+0E4D,U+200C,U+200D '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=thai-lao-tibetan/Thai-subset.ttf
```

## Lao-subset.ttf

- Release archive: https://github.com/notofonts/lao/releases/download/NotoSansLao-v2.003/NotoSansLao-v2.003.zip
- Archive SHA256: `5a87c31b1a40ef8147c1e84437e5f0ceba2d4dbbfc0b56a65821ad29870da8c0`
- Source member: `NotoSansLao/hinted/ttf/NotoSansLao-Regular.ttf`
- Source face SHA256: `0a86e5e1ccfe34ca78c43fac6829dc751b42bcc469272a9a55325aae587bfbe7`
- License: Lao-OFL.txt
- Subset SHA256: `e480cd9fa6c257c9010b7ab958a4e1b7fc7e4b5f0405ab61e435cee6a59ba733`
- Glyphs: 19; references: 7; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansLao-Regular.ttf --unicodes=U+0020,U+0E81,U+0E9B,U+0EB0,U+0EB2,U+0EB3,U+0EB4,U+0EC8,U+0ECD,U+200C,U+200D '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=thai-lao-tibetan/Lao-subset.ttf
```

## Tibetan-subset.ttf

- Release archive: https://github.com/notofonts/tibetan/releases/download/NotoSerifTibetan-v2.103/NotoSerifTibetan-v2.103.zip
- Archive SHA256: `4fba4a43cd61e68bc5b3a496f708ebce2c1937df02f2c8e81c4272b2b060a896`
- Source member: `NotoSerifTibetan/hinted/ttf/NotoSerifTibetan-Regular.ttf`
- Source face SHA256: `ee97bf3dc56e813651db734c9f35f8f1d41e7e31acf5f7d893e64ad22b292446`
- License: Tibetan-OFL.txt
- Subset SHA256: `b595ceb32061e39c1e25f4c9d71e054c8af1493305f2d5dd8534a8b0dc7f0020`
- Glyphs: 62; references: 7; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSerifTibetan-Regular.ttf --unicodes=U+0020,U+0F40,U+0F66,U+0F72,U+0F74,U+0F7C,U+0F90,U+0FB1,U+0FB5,U+200C,U+200D '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=thai-lao-tibetan/Tibetan-subset.ttf
```
