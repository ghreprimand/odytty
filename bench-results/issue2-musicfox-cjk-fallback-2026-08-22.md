# Issue 2 reproduction: MusicFox CJK fallback memory

Status: reproduced on one non-benchmark Linux Wayland development host. This
record is a bounded application reproduction, not a comparative benchmark and
not a reproduction on the reporter's hardware or desktop environment.

## Inputs

- Reporter issue: <https://github.com/ghreprimand/odytty/issues/2>
- OdyTTY reference build: official v0.11.1 Linux archive, SHA-256
  `f2d93919296cf20bc42f018968068565e3060dec5f2a47fe8527ba13b9187117`.
- Fixed build: source revision
  `864194b02a4c1190b778d076174d4d155b1b7761`.
- Application: MusicFox v5.1.0 source tag,
  `ad6e3fa254761d9ca22cbc94175fed35946ebe49`; official release archive SHA-256
  `ac7e4c05140c0c861aa0fefdfd5e02f825a8a50c507a9457656847916ecea37a`.
- CJK fallback fixture: `NotoSansCJKsc-Regular.otf` from the public
  `notofonts/noto-cjk` `Sans2.004` tag, SHA-256
  `2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b`,
  Git blob `dc15562470b4f842321894787a0d066879ccff8b`.

MusicFox was built from its committed vendor tree because its official prebuilt
was incompatible with the host audio-library ABI. No system-wide font or
library installation was changed. The CJK font was available only through a
process-local font configuration.

## Method and limits

Both OdyTTY builds used the same non-benchmark Linux Wayland development host,
fixed window geometry, user configuration, MusicFox binary, CJK font fixture,
chart navigation, and capture procedure. The application displayed its Chinese
chart and lyric views. Captures were taken after the visible workload populated,
at the start and end of a 120-second non-visible-surface interval, and after a
30-second restore interval.

The non-visible interval was implemented by moving the window to an inactive
workspace. It tests an OdyTTY non-visible surface path, not the reporter's
desktop-specific minimize implementation. MusicFox album-cover graphics were
disabled by default, so this workload does not exercise a live Kitty-graphics
image path.

This experiment does not reproduce the reporter's desktop environment or
hardware. It does not replace the protocol's longer W7 retention workload.

## Results

| phase | v0.11.1 RSS | v0.11.1 heap | fixed build RSS | fixed build heap |
| --- | ---: | ---: | ---: | ---: |
| chart and lyrics visible | 1,411,383,296 B | 1,213,677,568 B | 222,130,176 B | 42,356,736 B |
| non-visible start | 1,412,456,448 B | 1,214,533,632 B | 223,272,960 B | 43,261,952 B |
| non-visible after 120 seconds | 1,412,456,448 B | 1,214,533,632 B | 223,272,960 B | 43,261,952 B |
| restored after 30 seconds | 1,412,456,448 B | 1,214,533,632 B | 223,649,792 B | 43,577,344 B |

The visible checkpoint is 1,189,253,120 bytes lower in RSS and 1,171,320,832
bytes lower in heap on the fixed build. Both builds were flat during the
measured non-visible interval after their initial glyph population.

The v0.11.1 behavior is consistent with retaining repeated parsed copies of a
CJK fallback face as new glyph coverage is resolved. The fixed build shares an
immutable parsed runtime fallback face. The result establishes a v0.11.1 defect
sufficient to produce the reported high-memory class on another Linux system.
It does not prove that the reporter had no additional desktop-, driver-, or
hardware-specific cause, and it does not establish a minimize-triggered leak.
