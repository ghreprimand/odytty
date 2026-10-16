# rtl shaping fixtures

Project-authored owner samples and OFL subset fonts. References use numeric glyph IDs and UTF-8 byte clusters, in font units, with RTL direction (the scripts' own), und language, cluster level 0 and the OpenType shaper.

Tools: hb-shape (HarfBuzz) 14.5.1; fontTools 4.63.0. Complete copyright and OFL notices are retained in each script-specific OFL file and in OFL.txt. Modified families use OdyTTY fixture names; original copyright and license records remain embedded.

corpus.json in the parent directory records the input scalars and source archive member for every face. Known engine-version differences remain in references with explicit notes.

## Regeneration

Place the original Regular source faces, using their recorded filenames, in a directory named source. Run from the parent s5b directory:

```sh
python3 generate_fixtures.py --sources source --group rtl
```

The generator verifies each source hash, runs the exact subsetting commands below, renames the modified family records, and checks source/subset glyph equivalence before writing references. Its name-table rename is part of regeneration. Hinting and glyph names are removed; all layout features remain eligible for closure.

## Hebrew-subset.ttf

- Release archive: https://github.com/notofonts/hebrew/releases/download/NotoSansHebrew-v3.001/NotoSansHebrew-v3.001.zip
- Archive SHA256: `df0a71814b4e63644cf40fcc4529111b61266b7a2dafbe95068b29a7520cc3cb`
- Source member: `NotoSansHebrew/hinted/ttf/NotoSansHebrew-Regular.ttf`
- Source face SHA256: `cdefaf8efd47045f6820928eba84db5bed7557539328952b5f828315485e02ee`
- License: Hebrew-OFL.txt
- Subset SHA256: `a4123d33d89222cb8afdba79c10ee3ced558e353e81e44491305c2a6175b0f1d`
- Glyphs: 12; references: 6; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansHebrew-Regular.ttf --unicodes=U+0020,U+05B7,U+05B8,U+05B9,U+05BC,U+05C1,U+05D0,U+05D1,U+05DC,U+05E9 '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=rtl/Hebrew-subset.ttf
```

## Kharoshthi-subset.ttf

- Release archive: https://github.com/notofonts/kharoshthi/releases/download/NotoSansKharoshthi-v2.004/NotoSansKharoshthi-v2.004.zip
- Archive SHA256: `efc5217c3504cbda2d852d8436233a2578c40b11601550944f9475af6e8a65a2`
- Source member: `NotoSansKharoshthi/hinted/ttf/NotoSansKharoshthi-Regular.ttf`
- Source face SHA256: `65962c55e28808d8677810f4afe9f3d7af57508a19632628376a34bb3ecfe0fb`
- License: Kharoshthi-OFL.txt
- Subset SHA256: `dcfa4198b4dfd718d3ae2cd4181b05913907992766bb96420ea887ef12ec1866`
- Glyphs: 19; references: 8; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansKharoshthi-Regular.ttf --unicodes=U+0020,U+200C,U+200D,U+10A01,U+10A02,U+10A05,U+10A0E,U+10A10,U+10A1F,U+10A2F,U+10A3F '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=rtl/Kharoshthi-subset.ttf
```
