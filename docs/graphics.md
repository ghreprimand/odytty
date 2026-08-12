# OdyTTY Graphics Protocol Support

OdyTTY renders inline images through three protocols: the **Kitty graphics
protocol** (APC-based), **Sixel** (DCS-based), and **iTerm2 inline images**
(`OSC 1337 ; File=`). All three land on the same
shared GPU image layer, so images compose with terminal text using z-order:
cell backgrounds → negative-z images → glyphs → non-negative-z images. The
default `z=0` therefore places an image above text.

## Contents

- [Kitty graphics protocol](#kitty-graphics-protocol)
- [Security posture for file-based transports](#security-posture-for-file-based-transports)
- [Sixel](#sixel)
- [iTerm2 inline images](#iterm2-inline-images)
- [Protocol availability](#protocol-availability)
- [In-app image viewer (lightbox)](#in-app-image-viewer-lightbox)
- [Try it](#try-it)
- [Color emoji segment](#color-emoji-segment)

---

## Kitty graphics protocol

### Supported actions and formats

| `a=` | Meaning | Status |
|------|---------|--------|
| `t`  | Transmit — store image without displaying it | ✅ supported |
| `T`  | Transmit and display — store and place at cursor | ✅ supported |
| `p`  | Display a previously transmitted image (by `i=`) without re-sending pixels | ✅ supported |
| `d`  | Delete placements (see delete specifiers below) | ✅ supported |
| `q`  | Query — validate control data and payload, no storage | ✅ supported |
| `f`  | Animation - transmit frame data for an existing image | ✅ supported |
| `a`  | Animation - control playback (state, current frame, gap, loops) | ✅ supported |
| `c`  | Animation - compose a rectangle of one frame onto another | ✅ supported |

The `U=1` key on `a=T` / `a=p` creates a *virtual placement* for Unicode
placeholder display instead of placing at the cursor; see the placeholder
section below.

| `f=` | Format | Status |
|------|--------|--------|
| `32` | Raw RGBA (4 bytes/pixel, base64-encoded) | ✅ supported |
| `24` | Raw RGB (3 bytes/pixel, expanded to RGBA internally) | ✅ supported |
| `100` | PNG still image — grayscale, grayscale+alpha, RGB, and RGBA color types; 16-bit samples normalized to 8-bit | ✅ supported |

**Indexed PNG** (palette color type) is accepted: the decoder normalizes
palette frames to 8-bit RGB/RGBA before they reach the image store, so an
indexed PNG transmits and displays like any other color type. **Multi-frame PNG
containers** (APNG) are not decoded: only the first frame of a PNG is read.
Animation is driven by the protocol's frame commands (`a=f`), not by animated
container formats.

### Transports

| `t=` | Transport | Status |
|------|-----------|--------|
| `d` (default) | Direct — payload is base64-encoded pixel data in the APC itself | ✅ supported |
| `f` | File — payload is a base64-encoded filesystem path | ✅ with security restrictions |
| `t` | Temp file — like `f`, deleted after read | ✅ with security restrictions |
| `s` | POSIX shared memory — payload is a base64-encoded segment name | ✅ on Unix; rejected as unsupported on Windows |

### Chunked transfer

Large payloads can be split across multiple APC commands using `m=1` (more
chunks follow) and `m=0` (final chunk). OdyTTY accumulates chunks under a
96 MiB encoded-payload cap. If the cap is exceeded the transmission is
rejected with an explicit error response; incomplete state is cleared.

### Image ids, placement ids, and display geometry

- **`s=`/`v=`** — source pixel width / height. Both are required for raw
  `f=24` and `f=32` payloads; omitting either returns `missing-dimensions`.
  They are optional for PNG, where a supplied mismatch is rejected.
- **`i=`** — image id assigned by the application. If omitted, one is
  auto-assigned.
- **`p=`** — placement id. A single image may have several named placements at
  once; re-using the same `(i=, p=)` in the active screen buffer replaces the
  previous placement rather than adding a second one. Placements without a `p=`
  always accumulate. Tracked in `a=T`/`a=p`, honored by `a=d` per-id deletes,
  and echoed in responses.
- **`c=`/`r=`** — display width in columns / height in rows (cell-box scaling).
  If omitted, cell extents are derived from the visible image region (the
  source crop when set, otherwise the full image) and the current cell size.
- **`x=`/`y=`/`w=`/`h=`** — source-rectangle crop, in pixels, into the
  transmitted image (left, top, width, height). A zero or omitted width/height
  means "use the rest of the image." On a placement command these are pixel
  coordinates; on a delete command (`d=p`/`d=P`) `x=`/`y=` are instead cell
  coordinates.
- **`X=`/`Y=`** — pixel offset of the image within its anchor cell.
- **`z=`** — placement z-index (signed). Negative-z placements render beneath
  the text layer; zero/positive-z placements render above it. The full render
  order is: background cell colors → negative-z images → glyphs → non-negative-z
  images. Placements with equal z-index keep transmission order.
- **`C=1`** — suppress cursor movement after `a=T`/`a=p`. Default (`C=0` or
  absent): cursor moves to the row below the image at column 0.

### Quiet modes

| `q=` | Behavior |
|------|----------|
| `0` or absent | Send `OK` or error response for every command |
| `1` | Suppress `OK` responses; error responses are still sent |
| `2` | Suppress all responses (both `OK` and errors) |

### Delete specifiers (`a=d`)

| `d=` | What is deleted | Uppercase (`D=`) variant |
|------|-----------------|--------------------------|
| `a`  | All placements in the active screen buffer | `A` also frees unreferenced image data |
| `i`  | Placements for image `i=` (optionally filtered by `p=`) | `I` also frees data |
| `c`  | Placements intersecting the current cursor cell | `C` also frees data |
| `p`  | Placements intersecting cell `x=`,`y=` (defaults to cursor) | `P` also frees data |

Lowercase specifiers delete placements only. Uppercase specifiers also free
stored image data once no remaining placements reference the image.

### Unicode placeholders (`U=1`)

Virtual placements are supported. `a=T,U=1` (or `a=p,U=1`) stores the image
and its cell-grid extent without drawing anything or moving the cursor; the
image then renders wherever the client prints the placeholder character
U+10EEEE carrying row/column combining diacritics. The image id comes from
the placeholder cell's foreground color (24-bit truecolor or 256-color
palette index, with an optional high byte in a third diacritic) and the
placement id from its underline color; omitted diacritics inherit
left-to-right per the protocol. Because position lives in the text itself,
placeholder images scroll, page into scrollback, and are erased or
overwritten exactly as text is — the placement mode TUI toolkits rely on.
Virtual placements require a nonzero `i=` id, are reachable by id-addressed
deletes (`d=i`/`d=I`) but not location-addressed ones, and count as
references for image garbage collection. Known deviation: tiles split the
image uniformly across the placeholder grid; Kitty letterboxes to preserve
aspect ratio.

### Animation

Animated images are one image id with a list of frames. Frame 1 is the image
transmitted the ordinary way (`a=t`/`a=T`); further frames arrive as `a=f`
commands and are composed onto a background canvas - a previous frame named by
`c=`, or a solid color from `Y=` - either alpha-blended (the default) or copied
over (`X=1`). `r=` edits an existing frame instead of appending one. Frame data
rides every transport and format still images use, chunked transfer included.

`a=a` controls playback: `s=1` stops, `s=2` runs and waits at the last frame for
more frames, `s=3` runs and loops; `c=` makes a frame current; `r=` with `z=`
sets one frame's gap; `v=` sets the loop count (`v=1` is infinite). `a=c`
composes a rectangle from one frame onto another. `d=f` / `d=F` deletes an
image's frames, leaving the still image and its placements in place.

Gaps behave as the protocol specifies: `z=0` is ignored, a positive `z=` is the
delay in milliseconds before the next frame, and a negative `z=` marks a
*gapless* frame that is never displayed and exists only as base data for frames
composed from it. Playback clamps the effective gap to the 10ms..60s range, so a
1ms gap cannot pin the render loop at its frame ceiling.

What animation costs and what it does not:

- **Frames share the image budget.** Frame pixels count against the same
  decoded-byte quota as still images (64 MiB by default) rather than a second
  one, and a per-image cap of 64 frames bounds the frame list. A frame that
  would exceed either is refused with `ENOSPC` - the image being animated is
  never evicted to make room for its own frames.
- **Idle cost is zero.** A session with no animated image schedules no timer and
  does no per-frame animation work; the checks are gated on the image store
  holding frames at all.
- **Only what you can see animates.** Playback advances animations referenced by
  a *visible* placement in the active pane. An animation in a background tab or
  pane, or scrolled out of the viewport, holds its frame and resumes from the
  live clock when it comes back into view rather than burning frames off-screen.
- **Reduced motion does not stop animations.** The `reduced_motion` setting
  governs OdyTTY's own decorative motion (cursor easing, trails, fades). An
  animated image is program output, so suppressing it would corrupt what the
  program is displaying rather than calm the interface.
- **Images displayed through Unicode placeholders animate too**, since
  placeholder cells resolve into the same visible-placement list playback reads.

Two deviations from the reference terminal are worth stating. Frames are stored
fully rendered rather than as replayable operations, which trades memory for a
frame flip that is a byte copy. The specification's own description of `a=c` is
internally inconsistent: its frame-key table reverses the prose and example,
and one prose sentence reverses the rectangle offsets used by the example and
key table. OdyTTY follows the worked example for both mappings: `r=` and `X/Y`
name the source, while `c=` and `x/y` name the destination.

### What is not yet supported
- **`I=` image numbers** - animation commands must address an image by `i=`;
  the image-number form is not accepted.
- **Payload compression (`o=z`)** — zlib-compressed payloads are not supported.
  The `o=` key is ignored, so compressed data is rejected as an invalid payload
  rather than decompressed.

---

## Security posture for file-based transports

File and shared-memory transports read host filesystem state based on bytes
arriving over the PTY. When a session runs over SSH, those bytes originate from
a remote host. OdyTTY applies restrictions deliberately stricter than the
reference Kitty terminal to limit what terminal output can instruct the host to
read. Named transports are disabled by default. Set
`kitty_named_transports = on` or `ODYTTY_KITTY_NAMED_TRANSPORTS=on` only when
the entire PTY session, including plain SSH output, is trusted with this local
host-I/O authority. With the gate off, `t=f`, `t=t`, and `t=s` are rejected
before file or shared-memory I/O; direct and chunked-inline transfers remain
available.

### Path allowlist (`t=f`, `t=t`)

`t=f` and `t=t` paths must resolve inside an allowlisted canonical temp
directory: `/tmp`, `/dev/shm`, or the resolved value of `$TMPDIR` on any
platform, plus the system temporary directory (`std::env::temp_dir()`) on
Windows. Paths outside this set are rejected
before any file is opened. A Kitty reply contains status only, never the file
bytes. The restriction still prevents terminal output from using `t=f` as a
local readability and image-decodability oracle or rendering an allowed local
image without separate user action.

The reference Kitty terminal allows `t=f` from any path. OdyTTY intentionally
accepts only approved temporary roots so that untrusted terminal output cannot
probe arbitrary local files through this channel — a stricter posture than
Kitty's.

### Symlink rejection (`O_NOFOLLOW`, Unix)

On Unix, files are opened with `O_NOFOLLOW`. A symlink inside `/tmp` pointing to
`/etc/shadow` or any other file is rejected at the kernel open call, before
any data is read. This eliminates the TOCTOU race between path validation and
the `open()` call.

The reference Kitty terminal follows symlinks. OdyTTY rejects them on Unix.
Windows uses a plain file open after canonical-path allowlist validation and
does not provide the Unix `O_NOFOLLOW` guarantee.

### Regular-file validation (`t=f`, `t=t`)

On Unix, OdyTTY opens candidate files nonblocking and verifies the opened handle
is a regular file before reading. FIFOs, devices, directories, and other special
objects are rejected without reading bytes, so a PTY request cannot block on a
named pipe. Windows retains its regular-file transport behavior; POSIX shared
memory has no Windows surface.

### Delete-before-decode for temp files (`t=t`)

For `t=t`, the full path must contain the reference protocol's
`tty-graphics-protocol` marker. A marked temp file is deleted immediately after
its safe regular-file read, before image decode. Unmarked and rejected objects
are never deleted.

### Validated `shm_unlink` (`t=s`, Unix)

POSIX shared memory objects are opened read-only, bounded, and validated before
their names are unlinked. An invalid or unreadable object retains its name.
Windows keeps `t=s` unsupported.

### Size cap before decode

All three file transports enforce the ImageStore limit on the raw read before
any decode is attempted. A file claiming to decode into a large image is
rejected at the read stage; the decoder is never given a hostile payload.

---

## Sixel

OdyTTY decodes the Sixel DCS data language as defined by the DEC VT340 and
extended by xterm and foot, covering the full set of features listed below.

> **Note — DA1 does not advertise Sixel.** OdyTTY's Primary Device Attributes
> reply is `CSI ? 1 ; 2 c` and does **not** include the `;4` Sixel attribute.
> Applications that gate Sixel output on a DA1 probe will not emit Sixel to
> OdyTTY; tools that emit unconditionally (such as `img2sixel`) work fine.
> The raster `"Pan;Pad`-form aspect/grid parameters (DEC `P1`/`P3`) are parsed
> but not honored.

### Supported features

| Feature | Status |
|---------|--------|
| Raster attribute header (`"Pan;Pad;Ph;Pv`) | ✅ |
| Color introducer — RGB (`#Pc;2;Px;Py;Pz`) | ✅ |
| Color introducer — HLS (`#Pc;1;Px;Py;Pz`) | ✅ |
| Repeat introducer (`!count byte`) | ✅ |
| Graphics carriage return (`$`) | ✅ |
| Graphics new band (`-`) | ✅ |
| Sixel data bytes (`0x3F`–`0x7E`, 6 vertical pixels per byte) | ✅ |
| VT340 16-color default palette | ✅ |
| Transparent background mode (`P2=1`) | ✅ |

Hard caps: maximum image size is 10,000 × 10,000 pixels or 40 million total
pixels (~152 MiB RGBA). Malformed or truncated input never panics — unknown
bytes are skipped and partial images are returned for whatever was decoded
before a truncation.

**Memory behavior.** Sixel decoding allocates lazily and stays bounded:

- Raster attribute declarations (`"Pan;Pad;Ph;Pv`) are cap-validated immediately
  but do not allocate the declared canvas — the pixel buffer fills lazily as
  sixel data is painted.
- A header-only stream returns an `Empty` result with zero pixel allocation.
- Row stride grows geometrically (amortized `O(area)`), so wide images decoded
  column-by-column do not incur `O(N²)` buffer re-layouts.
- The pixel and axis caps listed above are unchanged by these optimizations.

### DECSDM (private mode 80)

DECSDM controls cursor behavior after a Sixel image is displayed.

- **DECSDM reset — default (`CSI ? 80 l`)**: after a Sixel image the cursor
  moves to the row below the image at column 0. This is the behavior most
  modern applications and terminals expect.
- **DECSDM set (`CSI ? 80 h`)**: the cursor stays at its position when the
  image is rendered — the image anchors at the cursor and the cursor does not
  advance.

DECSDM resets to off on `RIS` and `DECSTR` along with all other resettable
terminal modes.

---

## iTerm2 inline images

The iTerm2 inline-image extension transmits a whole image *file* — PNG, JPEG,
or WebP — as base64 inside an OSC 1337 payload:

```text
OSC 1337 ; File = inline=1 ; width=40 ; preserveAspectRatio=1 : <base64> ST
```

Unlike Kitty (raw pixels or PNG over APC) and Sixel (a pixel data language),
this protocol hands the terminal a container file and lets it decode. OdyTTY
content-sniffs the container rather than trusting any declared type.

### Supported arguments

| Argument | Status | Notes |
|----------|--------|-------|
| `inline=1` | ✅ supported | Required. Without it nothing is displayed. |
| `inline=0` | ❌ never honored | A download request; see below. |
| `size=N` | ✅ supported | Declared file length, cross-checked against the decoded payload. A mismatch beyond a 3-byte slack rejects the command. |
| `width=` / `height=` | ✅ supported | Accepts `auto`, `N` (cells), `Npx` (pixels), and `N%` (percent of the screen). |
| `preserveAspectRatio=` | ✅ supported | Defaults to `1`. With `1`, one specified axis derives the other and two specified axes define a box the image is fitted inside. With `0` the values are used as given and the image stretches. |
| `name=` | ⚠️ parsed, not shown | Validated and ignored: OdyTTY has no downloads UI to display it in. |
| unknown arguments | ⚠️ ignored | Future iTerm2 keys degrade to "ignored", never to "image rejected". |

Supported containers are exactly PNG, JPEG, and WebP - the formats the build
enables in the `image` crate. Animated containers are decoded as one still
frame; Kitty protocol frame commands are a separate animation path. A container
OdyTTY cannot decode is rejected and nothing is drawn.

### Size ceiling

The payload rides inside the OSC accumulator, which is bounded at 128 KiB for
any single OSC. The practical ceiling is therefore roughly **96 KiB of encoded
file bytes** per image (128 KiB of base64 text, minus the argument text).

A command that reaches the accumulator cap is **rejected whole**: the
accumulator drops over-cap bytes, so what would arrive is a truncated file, and
decoding a half-transferred image is worse than drawing nothing. This matches
the APC rule the Kitty path follows. For larger images use the Kitty protocol,
whose APC buffer is 1 MiB and which supports chunked transmission.

### Cursor semantics

The image anchors at the cursor, and the cursor then moves to column 0 of the
row below the image — the same rule as Sixel under DECSDM reset, and what
iTerm2 itself does. There is no "stay put" variant in this protocol (Kitty's
`C=1` has no iTerm2 spelling). The extent is clamped to the screen: columns to
what remains right of the cursor, rows to the screen height.

### What is not supported

- **`inline=0` downloads.** OdyTTY never writes a file to disk on behalf of a
  terminal escape sequence. A non-inline `File=` command is parsed and dropped:
  no file is created, nothing is displayed.
- **`MultipartFile=` / `FilePart=` / `FileEnd=`** (the chunked download form)
  are unhandled and consumed without state, so an emitter using them produces
  no output rather than a partial image.
- **Non-image `File=` payloads** (the extension also carries arbitrary file
  downloads) are rejected by the container decode.

---

## Protocol availability

Sixel, iTerm2 inline images, and Kitty direct/chunked-inline graphics are
always active. Kitty named
file, temporary-file, and POSIX shared-memory transports are off by default and
share the reloadable `kitty_named_transports` policy gate described above.

---

## In-app image viewer (lightbox)

Separately from the escape-sequence protocols above, OdyTTY can open a resolved
image path directly from terminal output in an in-terminal lightbox overlay. It
is available when the `interactive_paths` master gate is on. The
`interactive_paths_image_inline` sub-setting, which defaults on, controls only
the modifier-click shortcut; the right-click **Open in OdyTTY** entry remains
available while the master gate is on.

- **Open it** by Ctrl+clicking a detected `png` / `jpg` / `jpeg` / `webp` path
  on Linux/Windows, Cmd+clicking on macOS, or choosing **Open in OdyTTY** from
  the right-click menu.
- The file is decoded with the `image` crate and presented as a centered,
  scrim-dimmed overlay composited on top of the terminal.
- The overlay never upscales beyond the source pixels; dismiss it with `Esc` or
  by clicking outside the image.

This viewer is a distinct surface from the Kitty/Sixel inline placements: it is
driven by the path-interaction layer, not by bytes on the PTY. See
[keybindings.md](keybindings.md) for the platform-specific click chord and
[runtime-knobs.md](runtime-knobs.md) for the `interactive_paths` settings.

---

## Try it

### Kitty protocol

If you have Kitty installed, its `icat` kitten uses the Kitty graphics
protocol over the direct (`t=d`) transport:

```sh
kitty +kitten icat /path/to/image.png
```

`icat` sends PNG payloads (`f=100`) with `a=T` for transmit-and-display.
OdyTTY handles direct RGB, RGBA, and PNG transmission including chunked
transfers.

### iTerm2 inline images

iTerm2's `imgcat` script emits the `File=` form and works unchanged, as does
any tool that writes the sequence directly. From a shell:

```sh
printf '\033]1337;File=inline=1;width=40:%s\a' "$(base64 -w0 /path/to/image.png)"
```

Keep the encoded payload under ~96 KiB (see the size ceiling above); larger
images should go through the Kitty protocol instead.

### Sixel

`img2sixel` from the `libsixel` package renders images as Sixel DCS streams:

```sh
img2sixel /path/to/image.png
```

Install `libsixel` from your package manager:

```sh
# Debian / Ubuntu
apt install libsixel-bin

# Arch Linux
pacman -S libsixel
```

For a quick terminal test:

```sh
img2sixel --width=200 /path/to/image.png
```

---

## Color emoji segment

The native renderer owns a dedicated draw segment for premultiplied-RGBA color
glyphs, sitting between the coverage-text/decorations segment and the above-image
layer.

### Live pipeline

**Presentation policy.** For each terminal cell, `src/emoji/render.rs` decides
whether a grapheme should render as a color glyph or fall through to the
monochrome coverage path:

- **VS15 (`U+FE0E`)** anywhere in the grapheme → text presentation forced.
- **VS16 (`U+FE0F`)** anywhere in the grapheme → color presentation forced.
- No variation selector → color if the codepoint has the Unicode
  `Emoji_Presentation` default. That set is the two contiguous pictographic
  ranges `U+1F000`–`U+1FAFF` and `U+1FC00`–`U+1FFFD`, plus a curated list of
  individual emoji-default codepoints and small sub-ranges scattered through the
  `U+231A`–`U+2B55` symbol area (for example `U+231A`–`U+231B`,
  `U+2614`–`U+2615`, `U+2705`, `U+2728`, `U+2B1B`–`U+2B1C`, `U+2B50`). The whole
  `U+2600`–`U+26FF` / `U+2700`–`U+27BF` blocks are deliberately **not** treated
  as color-default — text-default symbols in those blocks (and the playback
  triangles `U+23F4`–`U+23F7`) fall through to the monochrome coverage path.
  Anything not in this set is text otherwise.

**Shaping.** Eligible graphemes are shaped with `swash` using Script=Common,
Direction=LTR, and the cell height as the pixel size. The shaper must produce
exactly one glyph id; if it produces zero (missing glyph) or more than one
(ligature sequence not yet handled), the cell falls back to the monochrome
coverage path without error.

**Rasterization.** The renderer prefers
`Source::ColorBitmap(StrikeWith::BestFit)`, then
`Source::ColorOutline(0)` for static COLR/CPAL v0 layers, then evaluates a
COLR v1 Paint graph through Fontations:

- The strike selection covers both CBDT/CBLC strikes (Noto Color Emoji on Linux)
  and sbix strikes (Apple Color Emoji on macOS).
- The color-outline source composites COLR v0 layers with CPAL palette zero,
  including compatible Segoe UI Emoji glyphs on Windows.
- The v1 evaluator supports solid fills, linear/radial/sweep gradients,
  transforms, clipping, nested color glyphs, and every standard composite
  mode. It writes premultiplied RGBA directly at the atlas-slot dimensions and
  engages only when the bitmap and v0 paths have no result.
- The returned image must have `Content::Color`; a monochrome strike causes the
  cell to fall back silently.
- The rendered image is scaled and centered into the atlas slot using
  nearest-neighbour resampling (aspect-ratio preserving, letterboxed). Bitmap
  straight alpha is converted to premultiplied RGBA; swash's composited COLR
  pixels are already premultiplied and are preserved.

**Atlas.** `ColorGlyphAtlas` (`src/emoji/color_atlas.rs`) is a grow-only
`Rgba8Unorm` atlas keyed by `(font identity, glyph-or-cluster id, physical px
size, scale)` — not by Unicode scalar — so ZWJ sequences, flags, keycap
sequences, and variation-selector variants are each cached by their shaped glyph
identity regardless of their codepoint count.

**Wide glyphs.** If the cell to the right carries a wide-continuation marker,
the lead cell's slot spans two cell widths. The continuation cell emits no
geometry; the atlas UV covers the full two-cell-wide bitmap in one quad.

**Monochrome suppression.** When a cell has a live color glyph run, the
monochrome coverage foreground quad is suppressed (`src/grid.rs`:
`build_cell_vertices_with_color_glyph_runs_into`). Backgrounds, decorations
(underline, strikethrough), and selection/search highlights are still emitted
so SGR styling layers correctly around the color bitmap without tinting it.

**Degradation.** If no supported color-emoji font is installed (Noto Color
Emoji, Apple Color Emoji, stock Windows Segoe UI Emoji, or another parseable
COLR/CPAL face), `EmojiRasterizer::discover()` returns a rasterizer with no font
rather than failing. A face or glyph with only SVG-in-OT data takes the
monochrome coverage path. Emoji cells remain readable. See
[accessibility.md](accessibility.md) for the related readability guarantees.
