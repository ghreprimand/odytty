// SPDX-License-Identifier: GPL-3.0-only
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
    /// painted sixel. Its physical layout is `cap_w` (row stride) × `cap_h`
    /// rows, both of which grow *geometrically* so that painting N columns
    /// costs amortized O(area) instead of O(N²). Raster-attribute declarations
    /// no longer pre-allocate — they only record `declared_w/declared_h` and
    /// validate them against the caps (see `raster_attrs`).
    rgba: Vec<u8>,
    /// Physical buffer width = row stride in pixels (capacity, not the drawn
    /// extent). Grows geometrically.
    cap_w: u32,
    /// Physical buffer height in rows (capacity, not the drawn extent). Grows
    /// geometrically.
    cap_h: u32,
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
    /// Max x+1 actually painted across all bands (drawn width).
    max_x: u32,
    /// Max y+1 (band bottom) actually reached by painting (drawn height).
    max_y: u32,
}

impl Decoder {
    fn new(background: SixelBackground) -> Self {
        let mut palette = Vec::with_capacity(16);
        palette.extend_from_slice(&VT340_PALETTE);
        Self {
            rgba: Vec::new(),
            cap_w: 0,
            cap_h: 0,
            declared_w: 0,
            declared_h: 0,
            x: 0,
            y: 0,
            color: 0,
            palette,
            background,
            has_data: false,
            max_x: 0,
            max_y: 0,
        }
    }

    /// Reject `(need_w, need_h)` if the *actual* drawn extent would exceed a
    /// hard cap. Checked against the real need (not the rounded-up capacity), so
    /// the cap semantics are identical to the pre-geometric decoder. No
    /// allocation happens here.
    fn check_caps(need_w: u32, need_h: u32) -> Result<(), SixelError> {
        if need_w > MAX_WIDTH || need_h > MAX_HEIGHT {
            return Err(SixelError::TooLarge {
                width: need_w,
                height: need_h,
            });
        }
        if (need_w as u64) * (need_h as u64) > MAX_PIXELS {
            return Err(SixelError::TooLarge {
                width: need_w,
                height: need_h,
            });
        }
        Ok(())
    }

    /// Geometric capacity step: double `cap` toward `need`, clamped to `limit`.
    fn grow_cap(cap: u32, need: u32, limit: u32) -> u32 {
        let mut c = cap.max(1);
        while c < need {
            c = c.saturating_mul(2);
        }
        c.min(limit).max(need)
    }

    /// Ensure the physical buffer can hold a pixel at column < `need_w`, row <
    /// `need_h`. Capacity grows geometrically in both axes; the row stride
    /// (`cap_w`) only changes O(log W) times, so the row re-layout it triggers
    /// is amortized O(area) over a full paint rather than O(W²). Returns `Err`
    /// (without allocating) when the real need exceeds a cap.
    fn ensure_capacity(&mut self, need_w: u32, need_h: u32) -> Result<(), SixelError> {
        Self::check_caps(need_w, need_h)?;
        if need_w <= self.cap_w && need_h <= self.cap_h {
            return Ok(());
        }
        let mut new_cap_w = Self::grow_cap(self.cap_w, need_w, MAX_WIDTH);
        let mut new_cap_h = Self::grow_cap(self.cap_h, need_h, MAX_HEIGHT);
        // Geometric rounding must never push the *capacity* past the pixel
        // budget. If it would, fall back to the tight need (guaranteed in-budget
        // because `check_caps` passed). Near the cap ceiling we lose the
        // geometric slack — bounded and rare.
        if (new_cap_w as u64) * (new_cap_h as u64) > MAX_PIXELS {
            new_cap_w = need_w.max(self.cap_w.min(MAX_WIDTH));
            new_cap_h = need_h.max(self.cap_h.min(MAX_HEIGHT));
            if (new_cap_w as u64) * (new_cap_h as u64) > MAX_PIXELS {
                new_cap_w = need_w;
                new_cap_h = need_h;
            }
        }

        if self.cap_w == 0 || self.cap_h == 0 {
            // First allocation.
            self.cap_w = new_cap_w;
            self.cap_h = new_cap_h;
            self.rgba
                .resize((new_cap_w as usize) * (new_cap_h as usize) * 4, 0);
            return Ok(());
        }
        if new_cap_w > self.cap_w {
            // Stride changed — re-layout existing rows into the wider buffer.
            // Geometric growth bounds this to O(log W) occurrences.
            let old_w = self.cap_w as usize;
            let old_h = self.cap_h as usize;
            let nw = new_cap_w as usize;
            let nh = new_cap_h as usize;
            let mut buf = vec![0u8; nw * nh * 4];
            for row in 0..old_h {
                let src = row * old_w * 4;
                let dst = row * nw * 4;
                buf[dst..dst + old_w * 4].copy_from_slice(&self.rgba[src..src + old_w * 4]);
            }
            self.rgba = buf;
            self.cap_w = new_cap_w;
            self.cap_h = new_cap_h;
        } else if new_cap_h > self.cap_h {
            // Stride unchanged — appending zero rows needs no row movement.
            self.rgba
                .resize((self.cap_w as usize) * (new_cap_h as usize) * 4, 0);
            self.cap_h = new_cap_h;
        }
        Ok(())
    }

    /// Paint one sixel column at (self.x, self.y). `bits` is the 6-bit value
    /// (sixel byte − 0x3F).
    fn paint_sixel(&mut self, bits: u8) -> Result<(), SixelError> {
        let band_bottom = self.y + 6;
        let need_w = self.x + 1;
        self.ensure_capacity(need_w, band_bottom)?;
        self.has_data = true;
        let [r, g, b] = self.current_rgb();
        for bit in 0..6u8 {
            let py = self.y + bit as u32;
            if py >= self.cap_h {
                break;
            }
            if bits & (1 << bit) != 0 {
                let idx = ((py as usize) * (self.cap_w as usize) + (self.x as usize)) * 4;
                if idx + 3 < self.rgba.len() {
                    self.rgba[idx] = r;
                    self.rgba[idx + 1] = g;
                    self.rgba[idx + 2] = b;
                    self.rgba[idx + 3] = 255;
                }
            }
        }
        if self.x + 1 > self.max_x {
            self.max_x = self.x + 1;
        }
        if band_bottom > self.max_y {
            self.max_y = band_bottom;
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
    ///
    /// Only *records* the declared image dimensions and validates them against
    /// the caps — it does **not** allocate. A header-only DCS stream
    /// (`"…Ph;Pv` with no sixel data) therefore costs nothing; the buffer is
    /// allocated lazily as pixels are painted, and the declared size is honored
    /// at `finish`. Over-cap declarations still fail fast with `TooLarge`,
    /// matching the pre-lazy decoder, but without the eager canvas allocation.
    fn raster_attrs(&mut self, params: &[u32]) -> Result<(), SixelError> {
        if params.len() >= 4 {
            let w = params[2].min(MAX_WIDTH);
            let h = params[3].min(MAX_HEIGHT);
            if w > 0 && h > 0 {
                Self::check_caps(w, h)?;
                self.declared_w = w;
                self.declared_h = h;
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

    fn finish(self) -> Result<SixelImage, SixelError> {
        if !self.has_data {
            return Err(SixelError::Empty);
        }
        // Final dimensions: the declared raster size is authoritative when
        // present (it both pads and crops the drawn extent); otherwise the
        // actually-drawn extent. Both are cap-safe — declared dims passed
        // `check_caps` in `raster_attrs`, and the drawn extent passed
        // `check_caps` on every painted column — so the single output
        // allocation below can never exceed the pixel budget.
        let final_w = if self.declared_w > 0 {
            self.declared_w
        } else {
            self.max_x.max(1)
        };
        let final_h = if self.declared_h > 0 {
            self.declared_h
        } else {
            self.max_y.max(1)
        };
        if final_w == 0 || final_h == 0 {
            return Err(SixelError::Empty);
        }

        let fw = final_w as usize;
        let fh = final_h as usize;
        let mut out = vec![0u8; fw * fh * 4];

        // Copy the painted region (the overlap of the final dimensions with the
        // physical capacity) row by row, translating from the capacity stride
        // (`cap_w`) to the tight output stride (`final_w`).
        let copy_w = final_w.min(self.cap_w) as usize;
        let copy_h = final_h.min(self.cap_h) as usize;
        if copy_w > 0 && copy_h > 0 && !self.rgba.is_empty() {
            let stride = self.cap_w as usize;
            for row in 0..copy_h {
                let src = row * stride * 4;
                let dst = row * fw * 4;
                out[dst..dst + copy_w * 4].copy_from_slice(&self.rgba[src..src + copy_w * 4]);
            }
        }

        // Opaque background: fill every still-transparent pixel (including the
        // padded region when the declared size exceeds the drawn extent) with
        // palette register 0.
        if self.background == SixelBackground::Opaque {
            let bg = self.palette.first().copied().unwrap_or([0, 0, 0]);
            for px in out.chunks_exact_mut(4) {
                if px[3] == 0 {
                    px[0] = bg[0];
                    px[1] = bg[1];
                    px[2] = bg[2];
                    px[3] = 255;
                }
            }
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
