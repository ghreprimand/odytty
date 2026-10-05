# southern-indic shaping fixtures

Project-authored owner samples and OFL subset fonts. References use numeric glyph IDs and UTF-8 byte clusters, in font units, with LTR direction, und language, cluster level 0 and the OpenType shaper.

Tools: hb-shape (HarfBuzz) 14.5.1; fontTools 4.63.0. Complete copyright and OFL notices are retained in each script-specific OFL file and in OFL.txt. Modified families use OdyTTY fixture names; original copyright and license records remain embedded.

corpus.json in the parent directory records the input scalars and source archive member for every face. Its explicit repertoire includes decomposition characters required for splitting vowels and SARA AM. Default-ignorable joiners remain in the input corpus. Known engine-version differences remain in references with explicit notes.

## Regeneration

Place the original Regular source faces, using their recorded filenames, in a directory named source. Run from the parent s5b directory:

```sh
python3 generate_fixtures.py --sources source --group southern-indic
```

The generator verifies each source hash, runs the exact subsetting commands below, renames the modified family records, and checks source/subset glyph equivalence before writing references. Its name-table rename is part of regeneration. Hinting and glyph names are removed; all layout features remain eligible for closure.

## Tamil-subset.ttf

- Release archive: https://github.com/notofonts/tamil/releases/download/NotoSansTamil-v2.004/NotoSansTamil-v2.004.zip
- Archive SHA256: `f8284e0f200a7f29a439b4ec88280d864b2b31f8479111c5b658ba6da38b3005`
- Source member: `NotoSansTamil/hinted/ttf/NotoSansTamil-Regular.ttf`
- Source face SHA256: `3c0a186feb3c63c7f6d63e1511dcdc144e745ae09b98e217c83f3e317974f6f9`
- License: Tamil-OFL.txt
- Subset SHA256: `6593378eaa6e41161cfd6f4df56aa4572f49568002434a9a680180092d70b6ff`
- Glyphs: 21; references: 9; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansTamil-Regular.ttf --unicodes=U+0020,U+0B95,U+0BB7,U+0BBE,U+0BBF,U+0BC6,U+0BC8,U+0BCA,U+0BCC,U+0BCD,U+0BD7,U+200C,U+200D '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=southern-indic/Tamil-subset.ttf
```

## Telugu-subset.ttf

- Release archive: https://github.com/notofonts/telugu/releases/download/NotoSansTelugu-v2.005/NotoSansTelugu-v2.005.zip
- Archive SHA256: `3553e00ca341dc06f4a143c604dd93a1342553169b5a06dc8b0ff50ab6eba0a2`
- Source member: `NotoSansTelugu/hinted/ttf/NotoSansTelugu-Regular.ttf`
- Source face SHA256: `b274780b69d1d23fe84b55e809a152cb2ac5306d33864b1f87622f6971871aae`
- License: Telugu-OFL.txt
- Subset SHA256: `5dc808a8a09ac4a8bc1fa5e4f62f91dd9dac3325ee2b9620ee6059d326be33fb`
- Glyphs: 55; references: 8; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansTelugu-Regular.ttf --unicodes=U+0020,U+0C15,U+0C30,U+0C37,U+0C3E,U+0C3F,U+0C46,U+0C4A,U+0C4D,U+200C,U+200D '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=southern-indic/Telugu-subset.ttf
```

## Kannada-subset.ttf

- Release archive: https://github.com/notofonts/kannada/releases/download/NotoSansKannada-v2.006/NotoSansKannada-v2.006.zip
- Archive SHA256: `902b1b92018f7c96862d68d0a39ef03b978203cd4c3f55d6760f597657b51c44`
- Source member: `NotoSansKannada/hinted/ttf/NotoSansKannada-Regular.ttf`
- Source face SHA256: `9ad74dc64838c6855b96f671fc08e425a58921b9d0c71712ea79c328a27e6e38`
- License: Kannada-OFL.txt
- Subset SHA256: `a6d0ae3c621df39bff15c7526024c3ba892c17820c92d74644176c5ab2e893f3`
- Glyphs: 210; references: 8; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansKannada-Regular.ttf --unicodes=U+0020,U+0C95,U+0CB0,U+0CB7,U+0CBE,U+0CBF,U+0CC2,U+0CC6,U+0CCA,U+0CCD,U+200C,U+200D '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=southern-indic/Kannada-subset.ttf
```

## Malayalam-subset.ttf

- Release archive: https://github.com/notofonts/malayalam/releases/download/NotoSansMalayalam-v2.104/NotoSansMalayalam-v2.104.zip
- Archive SHA256: `2ebd31e79f2893025d659def7784e0ec3557e7ff9ac105adcc82d35782913bf2`
- Source member: `NotoSansMalayalam/hinted/ttf/NotoSansMalayalam-Regular.ttf`
- Source face SHA256: `c08de7fa8d032a5d6a4d120fb82c78cec60b362a4e73fa26360d89759ff2a7f9`
- License: Malayalam-OFL.txt
- Subset SHA256: `ed5f013fe6215e20fff55c2d3777c76e91a1a1ec3edb09d6a8d46294ef5cea8e`
- Glyphs: 20; references: 8; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansMalayalam-Regular.ttf --unicodes=U+0020,U+0D15,U+0D30,U+0D37,U+0D3E,U+0D46,U+0D4A,U+0D4D,U+200C,U+200D '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=southern-indic/Malayalam-subset.ttf
```
