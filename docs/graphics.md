# OdyTTY Graphics Protocol Support

OdyTTY renders inline images through two protocols: the **Kitty graphics
protocol** (APC-based) and **Sixel** (DCS-based). Both land on the same
shared GPU image layer, so images compose with terminal text with correct
draw order (cell backgrounds → images → glyphs).

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

**Indexed PNG** (palette color type) is not supported and returns an explicit
error response. **PNG animation frames** are not supported — only the first
frame is decoded.

### Transports

| `t=` | Transport | Status |
|------|-----------|--------|
| `d` (default) | Direct — payload is base64-encoded pixel data in the APC itself | ✅ supported |
| `f` | File — payload is a base64-encoded filesystem path | ✅ with security restrictions |
| `t` | Temp file — like `f`, deleted after read | ✅ with security restrictions |
| `s` | POSIX shared memory — payload is a base64-encoded segment name | ✅ with security restrictions |

### Chunked transfer

Large payloads can be split across multiple APC commands using `m=1` (more
chunks follow) and `m=0` (final chunk). OdyTTY accumulates chunks under a
96 MiB encoded-payload cap. If the cap is exceeded the transmission is
rejected with an explicit error response; incomplete state is cleared.

### Image ids, placement ids, and display geometry

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
| `1` | Suppress `OK` responses; send errors |
| `2` | Suppress all responses (both `OK` and errors) |

### Delete specifiers (`a=d`)

| `d=` | What is deleted | Uppercase (`D=`) variant |
|------|-----------------|--------------------------|
| `a`  | All placements on the primary screen | `A` also frees unreferenced image data |
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

---

## Security posture for file-based transports

File and shared-memory transports read host filesystem state based on bytes
arriving over the PTY. When a session runs over SSH, those bytes originate from
a remote host. OdyTTY applies restrictions deliberately stricter than the
reference Kitty terminal to limit what a remote host can instruct the terminal
to read.

### Path allowlist (`t=f`, `t=t`)

`t=f` and `t=t` paths must resolve inside a canonical temp directory — `/tmp`,
`/dev/shm`, or the resolved value of `$TMPDIR`. Paths outside this set are
rejected before any file is opened. This blocks a remote-exfiltration attack
where the remote host constructs a `t=f` path pointing to `~/.ssh/id_rsa` or
another sensitive local file, causing the terminal to encode and return its
contents as pixel data.

The reference Kitty terminal allows `t=f` from any path. OdyTTY restricts to
temp directories because no legitimate application needs to transmit images
from outside a temp dir — programs that use file transports always write to a
temp file first.

### Symlink rejection (`O_NOFOLLOW`)

Files are opened with `O_NOFOLLOW`. A symlink inside `/tmp` pointing to
`/etc/shadow` or any other file is rejected at the kernel open call, before
any data is read. This eliminates the TOCTOU race between path validation and
the `open()` call.

The reference Kitty terminal follows symlinks. OdyTTY does not.

### Delete-before-decode for temp files (`t=t`)

For `t=t`, the temp file is deleted from the filesystem immediately after
reading, before any decode step. If a subsequent decode step fails, the file
is already gone and no data lingers on disk. This matches the Kitty
specification's documented "terminal should delete" semantics and adds the
guarantee that a decode failure cannot leave sensitive content behind.

### Immediate `shm_unlink` (`t=s`)

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

OdyTTY supports the complete Sixel DCS data language as defined by the DEC
VT340 and extended by xterm and foot.

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
