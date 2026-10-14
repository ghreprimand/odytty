# Internal benchmark fonts

Noto Sans Balinese and Noto Sans Syriac, SIL Open Font License 1.1.
The matching license files are retained beside the subsets.
Source: https://github.com/notofonts/noto-fonts/tree/ffebf8c1ee449e544955a7e813c54f9b73848eac/hinted/ttf

Subset with fontTools 4.63.0, all layout features and name records retained.
The subset requests ASCII U+0020-U+007E and retains only mappings present
in the source face (two in Balinese, fourteen in Syriac). Neither face maps
M: these direct API fixtures use the atlas's .notdef advance for cell width,
not a selectable application monospace font. Balinese also retains
U+1B13, U+1B35, U+1B36, U+1B44; Syriac also retains U+0710, U+0712,
U+0715, U+0730. These are benchmark faces, never application fallback fonts.
Project-authored scalar sequences appear in `benches/perf/protocol.rs`.

To reproduce a subset from the pinned upstream source with fontTools 4.63.0:

```sh
pyftsubset NotoSansBalinese-Regular.ttf --output-file=Balinese-subset.ttf \
  --unicodes=U+0020-007E,U+1B13,U+1B35,U+1B36,U+1B44 \
  --layout-features='*' --name-IDs='*' --name-languages='*' --name-legacy
pyftsubset NotoSansSyriac-Regular.ttf --output-file=Syriac-subset.ttf \
  --unicodes=U+0020-007E,U+0710,U+0712,U+0715,U+0730 \
  --layout-features='*' --name-IDs='*' --name-languages='*' --name-legacy
```
