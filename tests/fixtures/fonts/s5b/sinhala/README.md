# sinhala shaping fixtures

Project-authored owner samples and OFL subset fonts. References use numeric glyph IDs and UTF-8 byte clusters, in font units, with LTR direction, und language, cluster level 0 and the OpenType shaper.

Tools: hb-shape (HarfBuzz) 14.5.1; fontTools 4.63.0. Complete copyright and OFL notices are retained in each script-specific OFL file and in OFL.txt. Modified families use OdyTTY fixture names; original copyright and license records remain embedded.

corpus.json in the parent directory records the input scalars and source archive member for every face. Its explicit repertoire includes decomposition characters required for splitting vowels and SARA AM. Default-ignorable joiners remain in the input corpus. Known engine-version differences remain in references with explicit notes.

## Regeneration

Place the original Regular source faces, using their recorded filenames, in a directory named source. Run from the parent s5b directory:

```sh
python3 generate_fixtures.py --sources source --group sinhala
```

The generator verifies each source hash, runs the exact subsetting commands below, renames the modified family records, and checks source/subset glyph equivalence before writing references. Its name-table rename is part of regeneration. Hinting and glyph names are removed; all layout features remain eligible for closure.

## Sinhala-subset.ttf

- Release archive: https://github.com/notofonts/sinhala/releases/download/NotoSansSinhala-v3.000/NotoSansSinhala-v3.000.zip
- Archive SHA256: `25b2c787e0b34b82ccedce026e6519cc535394a6bd8520d5a1df45c233807d8a`
- Source member: `NotoSansSinhala/hinted/ttf/NotoSansSinhala-Regular.ttf`
- Source face SHA256: `9e32612d47004552f3125e78648a9e2e7899a216ccd3cefbb93a9b5f4c809feb`
- License: Sinhala-OFL.txt
- Subset SHA256: `2329b4dfe39b95198a0ba4761476d4fa02557864e516c4aed7c46c3638fd57bf`
- Glyphs: 26; references: 8; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansSinhala-Regular.ttf --unicodes=U+0020,U+0D9A,U+0DBB,U+0DC1,U+0DC2,U+0DCA,U+0DCF,U+0DD3,U+0DD9,U+0DDC,U+200C,U+200D '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=sinhala/Sinhala-subset.ttf
```
