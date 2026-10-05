# khmer-myanmar shaping fixtures

Project-authored owner samples and OFL subset fonts. References use numeric glyph IDs and UTF-8 byte clusters, in font units, with LTR direction, und language, cluster level 0 and the OpenType shaper.

Tools: hb-shape (HarfBuzz) 14.5.1; fontTools 4.63.0. Complete copyright and OFL notices are retained in each script-specific OFL file and in OFL.txt. Modified families use OdyTTY fixture names; original copyright and license records remain embedded.

corpus.json in the parent directory records the input scalars and source archive member for every face. Its explicit repertoire includes decomposition characters required for splitting vowels and SARA AM. Default-ignorable joiners remain in the input corpus. Known engine-version differences remain in references with explicit notes.

## Regeneration

Place the original Regular source faces, using their recorded filenames, in a directory named source. Run from the parent s5b directory:

```sh
python3 generate_fixtures.py --sources source --group khmer-myanmar
```

The generator verifies each source hash, runs the exact subsetting commands below, renames the modified family records, and checks source/subset glyph equivalence before writing references. Its name-table rename is part of regeneration. Hinting and glyph names are removed; all layout features remain eligible for closure.

## Khmer-subset.ttf

- Release archive: https://github.com/notofonts/khmer/releases/download/NotoSansKhmer-v2.004/NotoSansKhmer-v2.004.zip
- Archive SHA256: `19382ca97d62febea1c735ebee35a5aa4f03beca9b6ea6f6d86b7a7a0025a688`
- Source member: `NotoSansKhmer/hinted/ttf/NotoSansKhmer-Regular.ttf`
- Source face SHA256: `e66675f2082788f0511a714bef5a1748928294b38c8e286a96ea73a864b5e605`
- License: Khmer-OFL.txt
- Subset SHA256: `08344151046297cfd2c8c16720bb1181181825296cfccb437fc85e7a5ea0ec93`
- Glyphs: 18; references: 9; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansKhmer-Regular.ttf --unicodes=U+0020,U+1780,U+179A,U+17B6,U+17BE,U+17C1,U+17D2,U+200C,U+200D,U+25CC --layout-features=* --no-hinting --no-glyph-names --no-recalc-timestamp --name-IDs=* --name-languages=* --output-file=khmer-myanmar/Khmer-subset.ttf
```

## Myanmar-subset.ttf

- Release archive: https://github.com/notofonts/myanmar/releases/download/NotoSansMyanmar-v2.107/NotoSansMyanmar-v2.107.zip
- Archive SHA256: `c4995ee97f1f267b46cf83734dbf18a3cfd431e387b6fe38e90279546f260c4b`
- Source member: `NotoSansMyanmar/hinted/ttf/NotoSansMyanmar-Regular.ttf`
- Source face SHA256: `fafce4db400bc0b214907ccdbfb0ad2f18a57bfefd08c8a571830b84088cf2fc`
- License: Myanmar-OFL.txt
- Subset SHA256: `37ea46c5f90d3d6c5968b5b62af8458727e82c634d8947f3fda02f10be9444ca`
- Glyphs: 34; references: 10; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansMyanmar-Regular.ttf --unicodes=U+0020,U+1000,U+1004,U+102B,U+102C,U+102D,U+1031,U+1039,U+103A,U+103C,U+200C,U+200D --layout-features=* --no-hinting --no-glyph-names --no-recalc-timestamp --name-IDs=* --name-languages=* --output-file=khmer-myanmar/Myanmar-subset.ttf
```

## Stray-mark coverage

Khmer retains U+25CC so the lone U+17C1 reference inserts a dotted circle. Myanmar deliberately omits U+25CC so the lone U+1031 reference remains one glyph. The upstream Myanmar face maps U+25CC; for only the no-circle reference, the generator compares with a temporary source copy with that cmap mapping removed. All existing rows and the Khmer stray-mark row compare with the untouched source face. Glyph renumbering preserves existing glyph outlines, advances, offsets and UTF-8 clusters.
