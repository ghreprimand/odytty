# brahmic-b shaping fixtures

Project-authored owner samples and OFL subset fonts. References use numeric glyph IDs and UTF-8 byte clusters, in font units, with LTR direction, und language, cluster level 0 and the OpenType shaper.

Tools: hb-shape (HarfBuzz) 14.5.1; fontTools 4.63.0. Complete copyright and OFL notices are retained in each script-specific OFL file and in OFL.txt. Modified families use OdyTTY fixture names; original copyright and license records remain embedded.

corpus.json in the parent directory records the input scalars and source archive member for every face. Known engine-version differences remain in references with explicit notes.

## Regeneration

Place the original Regular source faces, using their recorded filenames, in a directory named source. Run from the parent s5b directory:

```sh
python3 generate_fixtures.py --sources source --group brahmic-b
```

The generator verifies each source hash, runs the exact subsetting commands below, renames the modified family records, and checks source/subset glyph equivalence before writing references. Its name-table rename is part of regeneration. Hinting and glyph names are removed; all layout features remain eligible for closure.

## Newa-subset.ttf

- Release archive: https://github.com/notofonts/newa/releases/download/NotoSansNewa-v2.007/NotoSansNewa-v2.007.zip
- Archive SHA256: `24b4301275f4188609da24a809742a5f0e3a159a3617ae1cc3642fefbfdec045`
- Source member: `NotoSansNewa/hinted/ttf/NotoSansNewa-Regular.ttf`
- Source face SHA256: `3aede9a3ab5740babffe75f7c0d97e2329734b8cb470c999be1871362084a8bd`
- License: Newa-OFL.txt
- Subset SHA256: `882defe177c96fe16dd6c6dd8cb54225d43ab36eb74d4f889f73230d58e5d3ef`
- Glyphs: 52; references: 8; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansNewa-Regular.ttf --unicodes=U+0020,U+200C,U+200D,U+1140E,U+1141F,U+1142C,U+11436,U+11438,U+11440,U+11442 '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=brahmic-b/Newa-subset.ttf
```

## Sharada-subset.ttf

- Release archive: https://github.com/notofonts/sharada/releases/download/NotoSansSharada-v2.006/NotoSansSharada-v2.006.zip
- Archive SHA256: `b7234ea126d00e31c7c7dfc614a117bf348449492168e685e1002df2e684deb0`
- Source member: `NotoSansSharada/hinted/ttf/NotoSansSharada-Regular.ttf`
- Source face SHA256: `8350a4cf4b5bdc0707aa5077075b0885a4f3f0f08ed6b49b8e5d142aef4032da`
- License: Sharada-OFL.txt
- Subset SHA256: `6f49a81bf02a13f705ac9b569cd50da2e7c5a3e6abe80a772f6198ccad130929`
- Glyphs: 24; references: 6; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansSharada-Regular.ttf --unicodes=U+0020,U+11191,U+111A0,U+111AB,U+111B4,U+111B6,U+111BE,U+111C0 '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=brahmic-b/Sharada-subset.ttf
```

## Tirhuta-subset.ttf

- Release archive: https://github.com/notofonts/tirhuta/releases/download/NotoSansTirhuta-v2.003/NotoSansTirhuta-v2.003.zip
- Archive SHA256: `ac5a1aa7f1bdf54d7b3bb1697c7825e99709b237ab194de1ea0e0ea36ba65352`
- Source member: `NotoSansTirhuta/hinted/ttf/NotoSansTirhuta-Regular.ttf`
- Source face SHA256: `f7a0d9fa2557ebebbacdc8888556e217c298eb049db6ff6dc210e916a1d9e860`
- License: Tirhuta-OFL.txt
- Subset SHA256: `671eb30275d733b55dc8a8f9aaa00bc917f3bb568f9b6d30ec0a01dc29b78e03`
- Glyphs: 30; references: 6; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansTirhuta-Regular.ttf --unicodes=U+0020,U+1148F,U+1149E,U+114A9,U+114B0,U+114B1,U+114B3,U+114B9,U+114BC,U+114C2 '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=brahmic-b/Tirhuta-subset.ttf
```

## Siddham-subset.ttf

- Release archive: https://github.com/notofonts/siddham/releases/download/NotoSansSiddham-v2.005/NotoSansSiddham-v2.005.zip
- Archive SHA256: `729bc8a1558826dcb0a05cda0e110de9c3b029610f613ab5edf8350a19af0253`
- Source member: `NotoSansSiddham/hinted/ttf/NotoSansSiddham-Regular.ttf`
- Source face SHA256: `a384592b36df5a2f283610c93703bcfb72e6fd86bd2e0a699da1eff91b247e88`
- License: Siddham-OFL.txt
- Subset SHA256: `676c6ab1a9cfa89ecad63c981617207ac4f37fd9a66a0c4ddb067fb62e299cef`
- Glyphs: 56; references: 6; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansSiddham-Regular.ttf --unicodes=U+0020,U+1158E,U+1159F,U+115A8,U+115AF,U+115B0,U+115B2,U+115B8,U+115BA,U+115BF '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=brahmic-b/Siddham-subset.ttf
```

## Takri-subset.ttf

- Release archive: https://github.com/notofonts/takri/releases/download/NotoSansTakri-v2.005/NotoSansTakri-v2.005.zip
- Archive SHA256: `0f51b6b1cf964c0703abbcfea168a78ad02fe3fa6332d9ea63c8aee9be9d7566`
- Source member: `NotoSansTakri/hinted/ttf/NotoSansTakri-Regular.ttf`
- Source face SHA256: `c14fb3d442ee9330d54d22aed37a9ee78ec4038055faa195d62661e4bb9988f9`
- License: Takri-OFL.txt
- Subset SHA256: `10caef6d886c55b2dd400500d1007f6ae0713aa34b17b1a4ef63e0e6e66377ac`
- Glyphs: 13; references: 6; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansTakri-Regular.ttf --unicodes=U+0020,U+1168A,U+11699,U+116A4,U+116AD,U+116AE,U+116B0,U+116B2,U+116B6 '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=brahmic-b/Takri-subset.ttf
```

## Ahom-subset.ttf

- Release archive: https://github.com/notofonts/ahom/releases/download/NotoSerifAhom-v2.007/NotoSerifAhom-v2.007.zip
- Archive SHA256: `7416ce83658f61cdaf836e91a12fb27d015eca824b283e3d5a3b68e351db4998`
- Source member: `NotoSerifAhom/hinted/ttf/NotoSerifAhom-Regular.ttf`
- Source face SHA256: `8153650d7fc1362a40b6cb355291e1d2718d82835790cd542a2521cba53c8fd4`
- License: Ahom-OFL.txt
- Subset SHA256: `b2f22c082db6dd1ec58e0999f72986d943cd1d12f42c48367908672b6f76bedd`
- Glyphs: 10; references: 6; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSerifAhom-Regular.ttf --unicodes=U+0020,U+11700,U+1171D,U+1171E,U+11722,U+11724,U+11726,U+1172B '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=brahmic-b/Ahom-subset.ttf
```

## TaiViet-subset.ttf

- Release archive: https://github.com/notofonts/tai-viet/releases/download/NotoSansTaiViet-v2.004/NotoSansTaiViet-v2.004.zip
- Archive SHA256: `615ea5c07d0da2fcb439ed56488283dd4af9f5d048c6c7044421cf50c9876d4f`
- Source member: `NotoSansTaiViet/hinted/ttf/NotoSansTaiViet-Regular.ttf`
- Source face SHA256: `5f7e2a8d5cca567620d101534fc34b432e1577ff01f3f1752750050c90b1ca7f`
- License: TaiViet-OFL.txt
- Subset SHA256: `7047e4a4dc9e98104a6f18b4fcb4d4f6d085bf422e7739f99c55580852752f4f`
- Glyphs: 9; references: 6; units per em: 1000.

Subsetting stage, followed by the generator name-table rename:

```sh
pyftsubset source/NotoSansTaiViet-Regular.ttf --unicodes=U+0020,U+AA80,U+AA81,U+AAB0,U+AAB2,U+AAB4,U+AABF,U+AAC1 '--layout-features=*' --no-hinting --no-glyph-names --no-recalc-timestamp '--name-IDs=*' '--name-languages=*' --output-file=brahmic-b/TaiViet-subset.ttf
```
