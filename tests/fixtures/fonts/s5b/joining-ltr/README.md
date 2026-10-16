# joining-ltr shaping fixtures

Project-authored joining-run samples and OFL subset fonts. References use numeric glyph IDs and UTF-8 byte clusters, in font units, with LTR direction (horizontal Mongolian), und language, cluster level 0 and the OpenType shaper.

Tools: hb-shape (HarfBuzz) 14.5.1; fontTools 4.63.0. Complete copyright and OFL notices are retained in each script-specific OFL file and in OFL.txt. Modified families use OdyTTY fixture names; original copyright and license records remain embedded.

corpus.json in the parent directory records the input scalars and source archive member for every face. Known engine-version differences remain in references with explicit notes.

## Regeneration

Place the original Regular source faces, using their recorded filenames, in a directory named source. Run from the parent s5b directory:

```sh
python3 generate_fixtures.py --sources source --group joining-ltr
```

The generator verifies each source hash, runs the exact subsetting commands below, renames the modified family records, and checks source/subset glyph equivalence before writing references. Its name-table rename is part of regeneration. Hinting and glyph names are removed; all layout features remain eligible for closure.

## Mongolian-subset.ttf

- Release archive: https://github.com/notofonts/mongolian/releases/download/NotoSansMongolian-v3.103/NotoSansMongolian-v3.103.zip
- Archive SHA256: `ab4ef0e294a7784e4a418523b0b214cda27dc876c03f7501f49ca8a8ba36756c`
- Source member: `NotoSansMongolian/hinted/ttf/NotoSansMongolian-Regular.ttf`
- Source face SHA256: `10f9066fe5cedcaa41c17d471bcadc7fb22880ce81ff2c06578671768ff44fbb`
- License: Mongolian-OFL.txt
- Subset SHA256: `21a4b7e8066c776f7e99c10aeb2327b2432caae670b11187648944acf5b22fad`
- Glyphs: 158; references: 7; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansMongolian-Regular.ttf --unicodes=U+0020,U+180B,U+180E,U+1820,U+1821,U+1822,U+1823,U+1824,U+1828,U+1829,U+182D,U+1885 '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=joining-ltr/Mongolian-subset.ttf
```
