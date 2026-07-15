# OdyTTY Graphics Protocol Support

OdyTTY renders inline images through two protocols: the **Kitty graphics
protocol** (APC-based) and **Sixel** (DCS-based). Both land on the same
shared GPU image layer, so images compose with terminal text using z-order:
cell backgrounds → negative-z images → glyphs → non-negative-z images. The
default `z=0` therefore places an image above text.

## Contents

- [Kitty graphics protocol](#kitty-graphics-protocol)
- [Security posture for file-based transports](#security-posture-for-file-based-transports)
- [Sixel](#sixel)
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
| `f`, `a` | Animation frame transmit / control | ❌ rejected (`unsupported-action`) |

| `f=` | Format | Status |
|------|--------|--------|
| `32` | Raw RGBA (4 bytes/pixel, base64-encoded) | ✅ supported |
| `24` | Raw RGB (3 bytes/pixel, expanded to RGBA internally) | ✅ supported |
| `100` | PNG still image — grayscale, grayscale+alpha, RGB, and RGBA color types; 16-bit samples normalized to 8-bit | ✅ supported |

**Indexed PNG** (palette color type) is accepted: the decoder normalizes
palette frames to 8-bit RGB/RGBA before they reach the image store, so an
indexed PNG transmits and displays like any other color type. **PNG animation
frames** are not supported — only the first frame is decoded.

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

### What is not yet supported

- **Animation** — only still images. Animation actions (`a=f`, `a=a`) are
  rejected with an `unsupported-action` response.
- **Unicode placeholder rendering** — image positioning via special Unicode
  codepoints in the cell grid. The placeholder key (`U=1`) is ignored and the
  image is placed at the cursor as usual.
- **Payload compression (`o=z`)** — zlib-compressed payloads are not supported.
  The `o=` key is ignored, so compressed data is rejected as an invalid payload
  rather than decompressed.

---

## Security posture for file-based transports

File and shared-memory transports read host filesystem state based on bytes
arriving over the PTY. When a session runs over SSH, those bytes originate from
a remote host. OdyTTY applies restrictions deliberately stricter than the
reference Kitty terminal to limit what a remote host can instruct the terminal
to read.

### Path allowlist (`t=f`, `t=t`)

`t=f` and `t=t` paths must resolve inside an allowlisted canonical temp
directory: `/tmp`, `/dev/shm`, or the resolved value of `$TMPDIR` on Unix, and
the system temporary directory on Windows. Paths outside this set are rejected
before any file is opened. This blocks a remote-exfiltration attack
where the remote host constructs a `t=f` path pointing to `~/.ssh/id_rsa` or
another sensitive local file, causing the terminal to encode and return its
contents as pixel data.

The reference Kitty terminal allows `t=f` from any path. OdyTTY restricts to
temp directories because no legitimate application needs to transmit images
from outside a temp dir — programs that use file transports always write to a
temp file first.

### Symlink rejection (`O_NOFOLLOW`, Unix)

On Unix, files are opened with `O_NOFOLLOW`. A symlink inside `/tmp` pointing to
`/etc/shadow` or any other file is rejected at the kernel open call, before
any data is read. This eliminates the TOCTOU race between path validation and
the `open()` call.

The reference Kitty terminal follows symlinks. OdyTTY rejects them on Unix.
Windows uses a plain file open after canonical-path allowlist validation and
does not provide the Unix `O_NOFOLLOW` guarantee.

### Delete-before-decode for temp files (`t=t`)

For `t=t`, the temp file is deleted from the filesystem immediately after
reading, before any decode step. If a subsequent decode step fails, the file
is already gone and no data lingers on disk. This matches the Kitty
specification's documented "terminal should delete" semantics and adds the
guarantee that a decode failure cannot leave sensitive content behind.

### Immediate `shm_unlink` (`t=s`, Unix)

POSIX shared memory objects are unlinked by name immediately after opening
(before data is read). The segment remains accessible via the open file
descriptor but can no longer be found by name, minimizing the window for a
squatting attack where a rogue process replaces the segment after validation
but before reading.

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

## Protocol availability

Both the Kitty graphics protocol and Sixel are always active — there is no
config key or `ODYTTY_*` environment variable to enable or disable either one.
Programs can emit images through whichever protocol they prefer without any
opt-in on the terminal side.

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

**Rasterization.** `swash` renders the glyph using `Source::ColorBitmap` with
`StrikeWith::BestFit`, requesting the embedded bitmap strike closest to the cell
height:

- The strike selection covers both CBDT/CBLC strikes (Noto Color Emoji on Linux)
  and sbix strikes (Apple Color Emoji on macOS).
- The returned image must have `Content::Color`; a monochrome strike causes the
  cell to fall back silently.
- The rendered bitmap is scaled and centered into the atlas slot using
  nearest-neighbour downscale (aspect-ratio preserving, letterboxed), then
  straight-alpha is converted to premultiplied RGBA before upload.

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
Emoji on Linux or Apple Color Emoji on macOS), `EmojiRasterizer::discover()`
returns a rasterizer with no font rather than failing. Stock Windows Segoe UI
Emoji is not discovered or rasterized, so Windows takes the same monochrome
coverage path. Emoji cells remain readable. See
[accessibility.md](accessibility.md) for the related readability guarantees.
