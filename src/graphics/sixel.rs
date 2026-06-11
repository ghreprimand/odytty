//! Sixel DCS payload decoder — raw DCS `q` body bytes to RGBA image.
//!
//! ## Input contract
//!
//! `decode_sixel` expects the **raw DCS `q` body** — everything after the `q`
//! final byte and before the String Terminator (ST). The DCS introducer
//! (`ESC P` / `0x90`), the parameters P1/P2/P3 preceding `q`, and the ST are
//! **not** part of this slice; the parser strips them. The P2 (background
//! select) parameter is passed separately.
//!
//! Concretely, if the wire carries `ESC P 0;1;0 q <body> ESC \`, the caller
//! passes `P2 = 1` and `payload = <body>`.
//!
//! ## Sixel data language (VT340 reference + xterm/foot modern practice)
//!
//! | Byte(s)       | Meaning |
//! |---------------|---------|
//! | `0x3F..=0x7E` | Sixel data character — 6 vertical pixels, value = byte − 0x3F |
//! | `!`           | Repeat introducer: `!<count><sixel_byte>` |
//! | `"`           | Raster attributes: `"Pan;Pad;Ph;Pv` |
//! | `#`           | Color introducer: `#Pc` (select) or `#Pc;Pu;Px;Py;Pz` (define + select) |
//! | `$`           | Graphics carriage return — rewind x to 0, stay on current band |
//! | `-`           | Graphics new line — rewind x to 0, advance y by 6 pixels |
//!
//! Unknown bytes outside `0x20..=0x7E` are silently skipped (robustness).
//!
//! ## Limits
//!
//! Hard caps prevent hostile streams from exhausting memory:
//! - Max image dimensions: 10 000 × 10 000 pixels.
//! - Max total pixel budget: 40 000 000 (≈ 152 MiB RGBA).
//! - Repeat counts are clamped to the remaining width.
//! - Color registers: 0..=1024.

use std::fmt;

// ---------------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------------

/// Maximum image width in pixels.
const MAX_WIDTH: u32 = 10_000;
/// Maximum image height in pixels.
const MAX_HEIGHT: u32 = 10_000;
/// Maximum total pixel count (width × height). ~152 MiB of RGBA.
const MAX_PIXELS: u64 = 40_000_000;
/// Maximum color register index.
const MAX_COLOR_REG: u16 = 1024;
/// Maximum numeric parameter value parsed from decimal digits.
const MAX_PARAM: u32 = 99_999_999;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A decoded Sixel image: width × height pixels in RGBA8 (premultiplied
/// alpha = 255 for all painted pixels).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SixelImage {
    pub width: u32,
    pub height: u32,
    /// Row-major RGBA8 pixel data. Length = `width * height * 4`.
    pub rgba: Vec<u8>,
}

/// Errors from Sixel decoding. Never panics — all malformed input is reported
/// here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SixelError {
    /// The payload is empty (no sixel data at all).
    Empty,
    /// Raster attributes or actual data exceed the hard pixel caps.
    TooLarge { width: u32, height: u32 },
}

impl fmt::Display for SixelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "empty sixel payload"),
            Self::TooLarge { width, height } => {
                write!(f, "sixel image too large: {width}×{height}")
            }
        }
    }
}

impl std::error::Error for SixelError {}

/// P2 background-select parameter from the DCS introducer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SixelBackground {
    /// P2 = 0 or 2 (default): zero-bit pixels are filled with the background
    /// color (palette register 0) so the image is fully opaque.
    #[default]
    Opaque,
    /// P2 = 1: zero-bit pixels remain transparent (alpha = 0). Useful for
    /// overlay compositing.
    Transparent,
}

impl SixelBackground {
    pub fn from_p2(p2: u16) -> Self {
        if p2 == 1 {
            Self::Transparent
        } else {
            Self::Opaque
        }
    }
}

// ---------------------------------------------------------------------------
// VT340 default palette (16 colors)
// ---------------------------------------------------------------------------

/// The VT340 default 16-color palette. Index 0 is black (background).
/// Colors are stored as `[R, G, B]` in 0–255 range. Derived from the
/// VT340 hardware palette (DEC percentage → 8-bit, rounded).
const VT340_PALETTE: [[u8; 3]; 16] = [
    [0, 0, 0],       // 0  black
    [51, 102, 204],  // 1  blue
    [204, 33, 33],   // 2  red
    [51, 204, 51],   // 3  green
    [204, 51, 204],  // 4  magenta
    [51, 204, 204],  // 5  cyan
    [204, 204, 51],  // 6  yellow
    [120, 120, 120], // 7  gray 50%
    [33, 33, 33],    // 8  dark gray 13%
    [84, 143, 255],  // 9  light blue
    [255, 84, 84],   // 10 light red
    [84, 255, 84],   // 11 light green
    [255, 84, 255],  // 12 light magenta
    [84, 255, 255],  // 13 light cyan
    [255, 255, 84],  // 14 light yellow
    [240, 240, 240], // 15 white
];

// ---------------------------------------------------------------------------
// Decoder state
// ---------------------------------------------------------------------------

struct Decoder {
    /// Pixel buffer (RGBA8). Grows lazily — starts empty, allocated on first
    /// sixel data or raster-attr declaration.
    rgba: Vec<u8>,
    /// Current buffer width (may grow as sixel data extends rightward).
    width: u32,
    /// Current buffer height (may grow as bands are added).
    height: u32,
    /// Declared width from raster attributes (0 = not declared).
    declared_w: u32,
    /// Declared height from raster attributes (0 = not declared).
    declared_h: u32,
    /// Current x position within the band (pixel column).
    x: u32,
    /// Current y position (top pixel row of the current 6-pixel band).
    y: u32,
    /// Active color register index.
    color: u16,
    /// Palette: index → [R, G, B]. Up to MAX_COLOR_REG + 1 entries.
    palette: Vec<[u8; 3]>,
    /// Background policy from the DCS P2 parameter.
    background: SixelBackground,
    /// Whether any sixel data byte has been painted (for Empty detection).
    has_data: bool,
    /// Max x seen across all bands (tracks actual drawn width).
    max_x: u32,
}

impl Decoder {
    fn new(background: SixelBackground) -> Self {
        let mut palette = Vec::with_capacity(16);
        palette.extend_from_slice(&VT340_PALETTE);
        Self {
            rgba: Vec::new(),
            width: 0,
            height: 0,
            declared_w: 0,
            declared_h: 0,
            x: 0,
            y: 0,
            color: 0,
            palette,
            background,
            has_data: false,
            max_x: 0,
        }
    }

    /// Ensure the pixel buffer covers at least (w, h). Grows width and/or
    /// height, re-laying out rows when the width increases. Returns `Err` on
    /// cap overflow.
    fn ensure_size(&mut self, w: u32, h: u32) -> Result<(), SixelError> {
        let new_w = w.max(self.width);
        let new_h = h.max(self.height);
        if new_w > MAX_WIDTH || new_h > MAX_HEIGHT {
            return Err(SixelError::TooLarge {
                width: new_w,
                height: new_h,
            });
        }
        if (new_w as u64) * (new_h as u64) > MAX_PIXELS {
            return Err(SixelError::TooLarge {
                width: new_w,
                height: new_h,
            });
        }
        if new_w == self.width && new_h == self.height {
            return Ok(());
        }
        if self.width == 0 || self.height == 0 {
            self.width = new_w;
            self.height = new_h;
            self.rgba.resize((new_w as usize) * (new_h as usize) * 4, 0);
            return Ok(());
        }
        if new_w > self.width {
            // Width grew — re-layout existing rows into a wider buffer.
            let old_w = self.width as usize;
            let old_h = self.height as usize;
            let nw = new_w as usize;
            let nh = new_h as usize;
            let mut buf = vec![0u8; nw * nh * 4];
            for row in 0..old_h {
                let src = row * old_w * 4;
                let dst = row * nw * 4;
                buf[dst..dst + old_w * 4].copy_from_slice(&self.rgba[src..src + old_w * 4]);
            }
            self.rgba = buf;
            self.width = new_w;
            self.height = nh as u32;
        } else if new_h > self.height {
            self.rgba
                .resize((self.width as usize) * (new_h as usize) * 4, 0);
            self.height = new_h;
        }
        Ok(())
    }

    /// Paint one sixel column at (self.x, self.y). `bits` is the 6-bit value
    /// (sixel byte − 0x3F).
    fn paint_sixel(&mut self, bits: u8) -> Result<(), SixelError> {
        self.has_data = true;
        let band_bottom = self.y + 6;
        let need_w = self.x + 1;
        self.ensure_size(need_w, band_bottom)?;
        let [r, g, b] = self.current_rgb();
        for bit in 0..6u8 {
            let py = self.y + bit as u32;
            if py >= self.height {
                break;
            }
            if bits & (1 << bit) != 0 {
                let idx = ((py as usize) * (self.width as usize) + (self.x as usize)) * 4;
                if idx + 3 < self.rgba.len() {
                    self.rgba[idx] = r;
                    self.rgba[idx + 1] = g;
                    self.rgba[idx + 2] = b;
                    self.rgba[idx + 3] = 255;
                }
            }
        }
        if self.x >= self.max_x {
            self.max_x = self.x + 1;
        }
        self.x += 1;
        Ok(())
    }

    fn current_rgb(&self) -> [u8; 3] {
        self.palette
            .get(self.color as usize)
            .copied()
            .unwrap_or([0, 0, 0])
    }

    /// Parse and apply raster attributes: `"Pan;Pad;Ph;Pv`.
    fn raster_attrs(&mut self, params: &[u32]) -> Result<(), SixelError> {
        if params.len() >= 4 {
            let w = params[2].min(MAX_WIDTH);
            let h = params[3].min(MAX_HEIGHT);
            if w > 0 && h > 0 {
                self.declared_w = w;
                self.declared_h = h;
                self.ensure_size(w, h)?;
            }
        }
        Ok(())
    }

    /// Parse and apply a color command: `#Pc` or `#Pc;Pu;Px;Py;Pz`.
    fn color_command(&mut self, params: &[u32]) {
        if params.is_empty() {
            return;
        }
        let reg = params[0] as u16;
        if reg > MAX_COLOR_REG {
            return;
        }
        if params.len() >= 5 {
            let pu = params[1];
            let px = params[2];
            let py = params[3];
            let pz = params[4];
            let rgb = match pu {
                2 => rgb_from_percent(px, py, pz),
                1 => hls_to_rgb(px, py, pz),
                _ => [0, 0, 0],
            };
            while self.palette.len() <= reg as usize {
                self.palette.push([0, 0, 0]);
            }
            self.palette[reg as usize] = rgb;
        }
        self.color = reg;
    }

    fn finish(mut self) -> Result<SixelImage, SixelError> {
        if !self.has_data {
            return Err(SixelError::Empty);
        }
        // Final dimensions: use declared raster size if present (clipped to
        // buffer), otherwise the actually-drawn extent.
        let final_w = if self.declared_w > 0 {
            self.declared_w.min(self.width)
        } else {
            self.max_x.min(self.width).max(1)
        };
        let final_h = if self.declared_h > 0 {
            self.declared_h.min(self.height)
        } else {
            self.height
        };
        if final_w == 0 || final_h == 0 {
            return Err(SixelError::Empty);
        }

        // Opaque background: fill zero-alpha pixels with palette register 0.
        if self.background == SixelBackground::Opaque {
            let bg = self.palette.first().copied().unwrap_or([0, 0, 0]);
            // Only fill within the final dimensions to avoid extra work.
            for row in 0..final_h as usize {
                let base = row * (self.width as usize) * 4;
                for col in 0..final_w as usize {
                    let idx = base + col * 4;
                    if idx + 3 < self.rgba.len() && self.rgba[idx + 3] == 0 {
                        self.rgba[idx] = bg[0];
                        self.rgba[idx + 1] = bg[1];
                        self.rgba[idx + 2] = bg[2];
                        self.rgba[idx + 3] = 255;
                    }
                }
            }
        }

        // Crop to final dimensions if smaller than buffer.
        if final_w == self.width && final_h == self.height {
            return Ok(SixelImage {
                width: final_w,
                height: final_h,
                rgba: self.rgba,
            });
        }
        let mut out = Vec::with_capacity((final_w as usize) * (final_h as usize) * 4);
        for row in 0..final_h {
            let src = (row as usize) * (self.width as usize) * 4;
            out.extend_from_slice(&self.rgba[src..src + (final_w as usize) * 4]);
        }
        Ok(SixelImage {
            width: final_w,
            height: final_h,
            rgba: out,
        })
    }
}

// ---------------------------------------------------------------------------
// Color helpers
// ---------------------------------------------------------------------------

/// Convert RGB percentages (0–100) to 8-bit [R, G, B].
pub(crate) fn rgb_from_percent(r: u32, g: u32, b: u32) -> [u8; 3] {
    [
        ((r.min(100) * 255 + 50) / 100) as u8,
        ((g.min(100) * 255 + 50) / 100) as u8,
        ((b.min(100) * 255 + 50) / 100) as u8,
    ]
}

/// Convert HLS (hue 0–360, lightness 0–100, saturation 0–100) to [R, G, B].
/// DEC's HLS uses H=0 for blue, rotating through red/green. We normalize to
/// standard HSL (H=0 = red) by rotating +240° mod 360.
pub(crate) fn hls_to_rgb(h: u32, l: u32, s: u32) -> [u8; 3] {
    let hue = ((h + 240) % 360) as f32;
    let light = (l.min(100) as f32) / 100.0;
    let sat = (s.min(100) as f32) / 100.0;
    if sat == 0.0 {
        let v = (light * 255.0 + 0.5) as u8;
        return [v, v, v];
    }
    let q = if light < 0.5 {
        light * (1.0 + sat)
    } else {
        light + sat - light * sat
    };
    let p = 2.0 * light - q;
    let hk = hue / 360.0;
    let channel = |t: f32| -> u8 {
        let t = if t < 0.0 {
            t + 1.0
        } else if t > 1.0 {
            t - 1.0
        } else {
            t
        };
        let val = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (val * 255.0 + 0.5) as u8
    };
    [
        channel(hk + 1.0 / 3.0),
        channel(hk),
        channel(hk - 1.0 / 3.0),
    ]
}

// ---------------------------------------------------------------------------
// Parameter parser
// ---------------------------------------------------------------------------

/// Parse a semicolon-delimited decimal parameter list from `payload[start..]`.
/// Returns `(params, next_index)` where `next_index` is the first byte that
/// is neither a digit nor a semicolon.
pub(crate) fn parse_params(payload: &[u8], start: usize) -> (Vec<u32>, usize) {
    let mut params = Vec::with_capacity(8);
    let mut val: u32 = 0;
    let mut has_val = false;
    let mut i = start;
    while i < payload.len() {
        let b = payload[i];
        if b.is_ascii_digit() {
            val = val.saturating_mul(10).saturating_add((b - b'0') as u32);
            if val > MAX_PARAM {
                val = MAX_PARAM;
            }
            has_val = true;
            i += 1;
        } else if b == b';' {
            params.push(if has_val { val } else { 0 });
            val = 0;
            has_val = false;
            i += 1;
        } else {
            break;
        }
    }
    if has_val || !params.is_empty() {
        params.push(if has_val { val } else { 0 });
    }
    (params, i)
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Decode a Sixel DCS `q` body into an RGBA image.
///
/// `payload` is the raw body after the `q` final byte and before the ST
/// (String Terminator). `background` is derived from the DCS P2 parameter
/// (see [`SixelBackground::from_p2`]).
///
/// # Errors
///
/// Returns [`SixelError::Empty`] if the payload contains no sixel data bytes,
/// and [`SixelError::TooLarge`] if the image would exceed the hard pixel caps.
///
/// # Panics
///
/// Never panics. Malformed input is handled gracefully: garbage bytes are
/// skipped, out-of-range parameters are clamped, truncated commands are
/// abandoned.
pub fn decode_sixel(payload: &[u8], background: SixelBackground) -> Result<SixelImage, SixelError> {
    if payload.is_empty() {
        return Err(SixelError::Empty);
    }
    let mut dec = Decoder::new(background);
    let mut i = 0;
    while i < payload.len() {
        match payload[i] {
            // Sixel data character: 6-bit column.
            b @ 0x3F..=0x7E => {
                dec.paint_sixel(b - 0x3F)?;
                i += 1;
            }
            // Repeat introducer: !<count><sixel_byte>
            b'!' => {
                i += 1;
                let (params, next) = parse_params(payload, i);
                i = next;
                let count = params.first().copied().unwrap_or(1).max(1);
                if i < payload.len() && (0x3F..=0x7E).contains(&payload[i]) {
                    let bits = payload[i] - 0x3F;
                    i += 1;
                    let safe = count.min(MAX_WIDTH);
                    for _ in 0..safe {
                        dec.paint_sixel(bits)?;
                    }
                }
            }
            // Raster attributes: "Pan;Pad;Ph;Pv
            b'"' => {
                i += 1;
                let (params, next) = parse_params(payload, i);
                i = next;
                dec.raster_attrs(&params)?;
            }
            // Color introducer: #Pc or #Pc;Pu;Px;Py;Pz
            b'#' => {
                i += 1;
                let (params, next) = parse_params(payload, i);
                i = next;
                dec.color_command(&params);
            }
            // Graphics carriage return.
            b'$' => {
                dec.x = 0;
                i += 1;
            }
            // Graphics new line.
            b'-' => {
                dec.x = 0;
                dec.y += 6;
                i += 1;
            }
            // Unknown / whitespace / control — skip.
            _ => {
                i += 1;
            }
        }
    }
    dec.finish()
}
