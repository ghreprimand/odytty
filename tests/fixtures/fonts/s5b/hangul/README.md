# hangul shaping fixtures

Project-authored owner samples and OFL subset fonts. References use numeric glyph IDs and UTF-8 byte clusters, in font units, with LTR direction, und language, cluster level 0 and the OpenType shaper.

Tools: hb-shape (HarfBuzz) 14.5.1; fontTools 4.63.0. Complete copyright and OFL notices are retained in each script-specific OFL file and in OFL.txt. Modified families use OdyTTY fixture names; original copyright and license records remain embedded.

corpus.json in the parent directory records the input scalars and source archive member for every face. Known engine-version differences remain in references with explicit notes.

## Regeneration

Place the original Regular source faces, using their recorded filenames, in a directory named source. Run from the parent s5b directory:

```sh
python3 generate_fixtures.py --sources source --group hangul
```

The generator verifies each source hash, runs the exact subsetting commands below, renames the modified family records, and checks source/subset glyph equivalence before writing references. Its name-table rename is part of regeneration. Hinting and glyph names are removed; all layout features remain eligible for closure.

## Hangul-subset.otf

- Release archive: https://github.com/notofonts/noto-cjk/releases/download/Sans2.004/17_NotoSansKR.zip
- Archive SHA256: `ac7eeb4e2b0d41de8ff31b2d6e1e2a41caf253fd5cefb380bfa1f40f1747b612`
- Source member: `NotoSansKR-Regular.otf`
- Source face SHA256: `69975a0ac8472717870aefeab0a4d52739308d90856b9955313b2ad5e0148d68`
- License: Hangul-OFL.txt
- Subset SHA256: `4009742f3651543341cb845c5bee85547e7ee56ca82efa152ad620782a9b1005`
- Glyphs: 64; references: 7; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansKR-Regular.otf --unicodes=U+0020,U+1100,U+110B,U+1112,U+1161,U+119E,U+11A2,U+11AB,U+A960,U+D7B0 '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=hangul/Hangul-subset.otf
```
