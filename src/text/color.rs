// SPDX-License-Identifier: GPL-3.0-only
//! Terminal color resolution: the default and ANSI palettes, the runtime
//! overrides the theme layer installs, and the sRGB/linear conversions the
//! render path uses.
//!
//! Core terminal semantics never read these -- only presentation does.

use std::sync::atomic::{AtomicU32, Ordering};

use crate::core::Color;

/// Default foreground (light gray) and background (near-black) in sRGB bytes.
///
/// These are the *baseline* defaults (the plain theme). The active default used
/// when resolving `Color::Default` is overridable at runtime via
/// [`set_default_colors`]; the theme layer sets it once at startup. Core terminal
/// semantics never read these — only presentation does.
pub const DEFAULT_FG_SRGB: (u8, u8, u8) = (0xCC, 0xCC, 0xCC);
pub const DEFAULT_BG_SRGB: (u8, u8, u8) = (0x0B, 0x0C, 0x10);

/// The historical xterm sRGB values for the 16 standard ANSI colors (indices
/// 0–7 normal, 8–15 bright). This is the *baseline* (plain theme) palette and
/// the source of truth pinned by tests: selecting `plain` (or no theme) renders
/// indexed colors byte-identically to the pre-theme appearance. The active ANSI
/// palette used to resolve [`Color::Indexed`] in the 0–15 range is overridable
/// at runtime via [`set_ansi_palette`]; the theme layer sets it once at startup.
/// The 256-color cube and grayscale ramp (indices 16–255) are computed and are
/// not theme-overridable.
pub const DEFAULT_ANSI_SRGB: [(u8, u8, u8); 16] = [
    (0x00, 0x00, 0x00), // 0  black
    (0xCD, 0x00, 0x00), // 1  red
    (0x00, 0xCD, 0x00), // 2  green
    (0xCD, 0xCD, 0x00), // 3  yellow
    (0x00, 0x00, 0xEE), // 4  blue
    (0xCD, 0x00, 0xCD), // 5  magenta
    (0x00, 0xCD, 0xCD), // 6  cyan
    (0xE5, 0xE5, 0xE5), // 7  white
    (0x7F, 0x7F, 0x7F), // 8  bright black
    (0xFF, 0x00, 0x00), // 9  bright red
    (0x00, 0xFF, 0x00), // 10 bright green
    (0xFF, 0xFF, 0x00), // 11 bright yellow
    (0x5C, 0x5C, 0xFF), // 12 bright blue
    (0xFF, 0x00, 0xFF), // 13 bright magenta
    (0x00, 0xFF, 0xFF), // 14 bright cyan
    (0xFF, 0xFF, 0xFF), // 15 bright white
];

/// Pack an sRGB triple into a `u32` for atomic storage (`0x00RRGGBB`).
pub(super) const fn pack_srgb(c: (u8, u8, u8)) -> u32 {
    ((c.0 as u32) << 16) | ((c.1 as u32) << 8) | (c.2 as u32)
}

/// Unpack a `0x00RRGGBB` value back into an sRGB triple.
fn unpack_srgb(v: u32) -> (u8, u8, u8) {
    (
        ((v >> 16) & 0xFF) as u8,
        ((v >> 8) & 0xFF) as u8,
        (v & 0xFF) as u8,
    )
}

/// Active default foreground/background for `Color::Default`, overridable by the
/// theme layer. Stored as packed sRGB so resolution stays lock-free. This is a
/// presentation-only override: it changes how `Color::Default` paints, never
/// what the terminal core stores.
static DEFAULT_FG: AtomicU32 = AtomicU32::new(pack_srgb(DEFAULT_FG_SRGB));
static DEFAULT_BG: AtomicU32 = AtomicU32::new(pack_srgb(DEFAULT_BG_SRGB));

/// Active 16-color ANSI palette for resolving `Color::Indexed(0..=15)`,
/// overridable by the theme layer. Stored as packed sRGB so resolution stays
/// lock-free, mirroring [`DEFAULT_FG`]/[`DEFAULT_BG`]. Presentation-only: this
/// changes how indexed colors paint, never what the terminal core stores. The
/// initial values are the historical xterm table ([`DEFAULT_ANSI_SRGB`]), so an
/// un-themed renderer is byte-identical to the pre-theme appearance.
static ANSI_PALETTE: [AtomicU32; 16] = [
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[0])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[1])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[2])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[3])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[4])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[5])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[6])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[7])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[8])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[9])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[10])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[11])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[12])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[13])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[14])),
    AtomicU32::new(pack_srgb(DEFAULT_ANSI_SRGB[15])),
];

/// Override the default foreground/background used to resolve `Color::Default`.
///
/// Called once at native startup by the theme layer. Affects only rendering;
/// the terminal model is unaware of it. Passing the baseline constants restores
/// the plain appearance.
pub fn set_default_colors(foreground: (u8, u8, u8), background: (u8, u8, u8)) {
    DEFAULT_FG.store(pack_srgb(foreground), Ordering::Relaxed);
    DEFAULT_BG.store(pack_srgb(background), Ordering::Relaxed);
}

/// Override the 16-color ANSI palette used to resolve `Color::Indexed(0..=15)`.
///
/// Called once at native startup by the theme layer (alongside
/// [`set_default_colors`]). Affects only rendering — the terminal model is
/// unaware of it — and is layered *below* any per-app OSC-4 dynamic-color
/// override: the render path consults the core dynamic palette first and only
/// falls back to [`indexed_srgb`] (which reads this override) when no app
/// override is set, so OSC-4 always wins over the theme. Passing
/// [`DEFAULT_ANSI_SRGB`] restores the plain appearance.
pub fn set_ansi_palette(palette: &[(u8, u8, u8); 16]) {
    for (slot, &color) in ANSI_PALETTE.iter().zip(palette.iter()) {
        slot.store(pack_srgb(color), Ordering::Relaxed);
    }
}

/// Test-only snapshot of every process-global color seam in this module.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ColorGlobals {
    pub(crate) default_fg: (u8, u8, u8),
    pub(crate) default_bg: (u8, u8, u8),
    pub(crate) ansi_palette: [(u8, u8, u8); 16],
}

/// Capture the process-global color seams so the shared render-globals guard
/// can hold a baseline and write it back verbatim. Restoration goes through the
/// public [`set_default_colors`] and [`set_ansi_palette`] setters, so there is
/// no second write path to keep in step with this reader.
#[cfg(test)]
pub(crate) fn color_globals_for_test() -> ColorGlobals {
    let mut ansi_palette = [(0u8, 0u8, 0u8); 16];
    for (index, slot) in ansi_palette.iter_mut().enumerate() {
        *slot = ansi_srgb(index as u8);
    }
    ColorGlobals {
        default_fg: default_fg_srgb(),
        default_bg: default_bg_srgb(),
        ansi_palette,
    }
}

fn default_fg_srgb() -> (u8, u8, u8) {
    unpack_srgb(DEFAULT_FG.load(Ordering::Relaxed))
}

fn default_bg_srgb() -> (u8, u8, u8) {
    unpack_srgb(DEFAULT_BG.load(Ordering::Relaxed))
}

/// The active sRGB bytes for a standard ANSI color index (0–15), reading the
/// runtime palette override.
fn ansi_srgb(index: u8) -> (u8, u8, u8) {
    unpack_srgb(ANSI_PALETTE[index as usize].load(Ordering::Relaxed))
}

/// Convert one sRGB channel byte to a linear float in `[0, 1]`.
///
/// The surface uses an sRGB texture format, which applies the linear→sRGB
/// transfer on write, so shader inputs must be linear.
///
/// This is a thin façade over [`crate::color::srgb_to_linear`], which is the
/// single source of truth for the transfer (RV3). The value is byte-identical
/// to the historical inline formula, so `native::gpu`, `grid`, and every other
/// caller see no change.
pub fn srgb_to_linear(byte: u8) -> f32 {
    crate::color::srgb_to_linear(byte)
}

/// Perceptually dim a linear-RGBA color, preserving alpha (RV3).
///
/// This is the render-facing adapter over [`crate::color::dim_perceptual`]:
/// SGR dim/faint should scale OKLab lightness rather than naively halving each
/// linear channel, which keeps dimmed text legible and hue-stable. `amount` is
/// in `[0, 1]`; `0.0` returns the input unchanged (exact identity), so the
/// default/plain path stays byte-identical until a caller opts in.
pub fn dim_linear_rgba(color: [f32; 4], amount: f32) -> [f32; 4] {
    let [r, g, b] = crate::color::dim_perceptual([color[0], color[1], color[2]], amount);
    [r, g, b, color[3]]
}

/// Active minimum fg/bg contrast floor (RV1), stored as the bit pattern of an
/// `f32` so resolution stays lock-free, mirroring the palette seams above.
///
/// `1.0` (the default) means "no floor" — [`enforce_contrast_rgba`] is then an
/// exact identity, so an un-configured renderer is byte-identical to before.
/// The native layer sets it from `Settings::min_contrast` at startup/reload.
pub(super) static MIN_CONTRAST: AtomicU32 = AtomicU32::new(1.0_f32.to_bits());

/// Override the minimum fg/bg contrast floor used by [`enforce_contrast_rgba`].
///
/// Presentation-only: it changes how text is painted to keep it legible, never
/// what the terminal core stores. `ratio <= 1.0` disables enforcement (exact
/// passthrough). Mirrors [`set_ansi_palette`]/[`set_default_colors`].
pub fn set_min_contrast(ratio: f32) {
    MIN_CONTRAST.store(ratio.to_bits(), Ordering::Relaxed);
}

/// The active minimum-contrast floor (`1.0` = disabled).
pub fn min_contrast() -> f32 {
    f32::from_bits(MIN_CONTRAST.load(Ordering::Relaxed))
}

/// Enforce the active minimum-contrast floor on a resolved linear-RGBA
/// foreground against its background, preserving alpha (RV1).
///
/// This is the render-facing seam over [`crate::color::enforce_min_contrast`]:
/// the caller passes the final per-cell `fg`/`bg` (after inverse/dim) and gets
/// back an `fg` whose WCAG contrast against `bg` meets at least the configured
/// floor, with hue preserved. When the floor is at its passthrough value
/// (`1.0`, the default) this returns `fg` unchanged, so the plain path stays
/// byte-identical until the floor is raised.
pub fn enforce_contrast_rgba(fg: [f32; 4], bg: [f32; 4]) -> [f32; 4] {
    let ratio = min_contrast();
    if ratio <= 1.0 {
        return fg;
    }
    let [r, g, b] =
        crate::color::enforce_min_contrast([fg[0], fg[1], fg[2]], [bg[0], bg[1], bg[2]], ratio);
    [r, g, b, fg[3]]
}

/// TEXT-BRIGHTNESS: lift a linear-RGBA glyph foreground toward white with a
/// soft knee, preserving alpha.
///
/// For in-gamut channels, `c' = 1 - (1 - c)^b` for `b >= 1.0`: identity at
/// `b == 1.0` (early-returned, exact — the plain path stays byte-identical),
/// monotonic in both the channel and the knob, and `c' < 1` whenever `c < 1`,
/// so near-white ink compresses smoothly instead of clipping flat and channel
/// ordering is preserved — colors lighten without fully desaturating. Black is
/// a fixed point: the curve lifts mid-tones and dim colors, not `#000` ink,
/// which would only lose contrast on light backgrounds.
///
/// Out-of-gamut channels are preserved exactly. The minimum-contrast floor can
/// produce values above `1.0`; those carry useful energy in the float scene
/// target used by bloom/CRT. Clamping them only when brightness is enabled
/// would make the raised setting darker than the identity path. Applied by the
/// vertex build AFTER [`enforce_contrast_rgba`], so a floor-corrected color is
/// the lift's input and the ramp cannot undo the floor's direction of correction.
pub fn lift_brightness_rgba(color: [f32; 4], brightness: f32) -> [f32; 4] {
    if brightness <= 1.0 {
        return color;
    }
    let lift = |c: f32| {
        if !(0.0..=1.0).contains(&c) {
            return c;
        }
        1.0 - (1.0 - c).powf(brightness)
    };
    [lift(color[0]), lift(color[1]), lift(color[2]), color[3]]
}

/// Linear-RGBA (opaque) for an sRGB triple.
fn linear_rgba(srgb: (u8, u8, u8)) -> [f32; 4] {
    [
        srgb_to_linear(srgb.0),
        srgb_to_linear(srgb.1),
        srgb_to_linear(srgb.2),
        1.0,
    ]
}

/// The sRGB bytes for an xterm 256-color palette index.
///
/// Indices 0–15 (the standard ANSI colors) read the active theme palette via
/// the [`set_ansi_palette`] override seam; with no override applied they return
/// the historical xterm values ([`DEFAULT_ANSI_SRGB`]). The 256-color cube and
/// grayscale ramp (16–255) are computed and not theme-overridable.
pub fn indexed_srgb(index: u8) -> (u8, u8, u8) {
    match index {
        // 16 standard ANSI colors — theme-overridable.
        0..=15 => ansi_srgb(index),
        // 6x6x6 color cube.
        16..=231 => {
            let i = index - 16;
            let r = i / 36;
            let g = (i % 36) / 6;
            let b = i % 6;
            let level = |v: u8| -> u8 { if v == 0 { 0 } else { 55 + v * 40 } };
            (level(r), level(g), level(b))
        }
        // 24-step grayscale ramp.
        232..=255 => {
            let v = 8 + (index - 232) * 10;
            (v, v, v)
        }
    }
}

/// Resolve a terminal foreground color to linear RGBA.
pub fn foreground_linear(color: Color) -> [f32; 4] {
    match color {
        Color::Default => linear_rgba(default_fg_srgb()),
        Color::Indexed(i) => linear_rgba(indexed_srgb(i)),
        Color::Rgb(r, g, b) => linear_rgba((r, g, b)),
    }
}

/// Resolve a terminal background color to linear RGBA.
pub fn background_linear(color: Color) -> [f32; 4] {
    match color {
        Color::Default => linear_rgba(default_bg_srgb()),
        Color::Indexed(i) => linear_rgba(indexed_srgb(i)),
        Color::Rgb(r, g, b) => linear_rgba((r, g, b)),
    }
}
