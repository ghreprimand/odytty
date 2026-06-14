// SPDX-License-Identifier: GPL-3.0-only
/// Human-readable help for the stem-darken knob, destined for the in-app
/// settings panel (UX2). Establishes the convention that every new knob ships
/// with a concise description, its accepted values, and its default.
pub const STEM_DARKEN_DESC: &str = "Stem darkening: boosts glyph coverage so light-on-dark text holds weight at \
     small sizes. Accepts 0.0–1.0; 0.0 is off (identical to no boost), 1.0 is \
     strongest. Default 0.2 (a subtle crispness boost).";

/// Human-readable help for the minimum-contrast knob, shown in the in-app
/// settings panel (UX2). Follows the every-knob-carries-a-description convention.
pub const MIN_CONTRAST_DESC: &str = "Minimum contrast: lifts foreground text so its WCAG contrast against the \
     background meets at least this ratio, keeping low-contrast apps legible. \
     Accepts 1.0–21.0; 1.0 is off (no change), 4.5 is the WCAG AA body-text \
     threshold, 7.0 is AAA. Hue is preserved. Default 1.0.";

/// Human-readable help for the focus-dimming knob (ID2), shown in the in-app
/// settings panel. Follows the every-knob-carries-a-description convention.
pub const FOCUS_DIM_DESC: &str = "Focus dimming: dims the whole window (text and background) while it is \
     unfocused so it recedes visually, in OKLab so hue is preserved. Accepts \
     0.0–1.0; 0.0 is off (no change, focused and unfocused look identical), \
     0.15–0.30 is a subtle recede. The focused window is never dimmed. The \
     minimum-contrast floor still applies, so text stays legible. Default 0.0.";

pub const RENDER_QUALITY_DESC: &str = "Render quality: master renderer profile. balanced is today's default; \
     plain is the hard fast path and forces post-process effects, focus dimming, \
     stem darkening, and the minimum-contrast floor off even when their individual \
     knobs are enabled. high is reserved for future higher-cost quality paths.";

pub const BLOOM_DESC: &str = "Bloom: optional HDR phosphor glow over bright cells. Off by default and \
     pixel-identical to the plain renderer. Requires a GPU with filterable \
     Rgba16Float render targets; unsupported adapters silently use the plain path.";
pub const BLOOM_THRESHOLD_DESC: &str = "Bloom threshold: linear luminance knee for the bright-pass. The default is \
     derived from the active theme foreground luminance plus a safety margin, so \
     normal body text sits below the knee and does not glow.";
pub const BLOOM_INTENSITY_DESC: &str = "Bloom intensity: additive glow strength. Accepts 0.0–1.0; 0.0 emits no \
     glow, 0.4 is the conservative default, and the cap keeps bloom bounded.";
pub const BLOOM_RADIUS_DESC: &str = "Bloom radius: blur spread in half-resolution pixels. Accepts 0.5–8.0; \
     3.0 is the default soft phosphor radius.";

pub const CRT_DESC: &str = "CRT profile: optional post-process scanlines plus vignette. Off by default and pixel-identical to the plain renderer. Requires the same adapter support as bloom; unsupported adapters silently use the plain path.";
pub const CRT_SCANLINE_INTENSITY_DESC: &str = "CRT scanline intensity: bounded multiplicative dimming for the dark part of each scanline. Accepts 0.0–0.18; the cap keeps text readable and prevents opaque overlays.";
pub const CRT_SCANLINE_PERIOD_DESC: &str = "CRT scanline period: vertical distance between scanline bands in physical pixels. Accepts 2.0–12.0; 3.0 is the conservative default.";
pub const CRT_VIGNETTE_STRENGTH_DESC: &str = "CRT vignette strength: bounded edge dimming. Accepts 0.0–0.16; the shader enforces a brightness floor so corners recede without erasing lit cells.";

/// Human-readable help for the geometric box-drawing knob (RV2), shown in the
/// in-app settings panel. Follows the every-knob-carries-a-description convention.
pub const GEOMETRIC_BOXDRAW_DESC: &str = "Geometric box-drawing: renders line, block and Powerline glyphs from \
     cell-aligned geometry instead of the font, so TUI borders, progress bars \
     and powerline prompts are pixel-perfect and seamless at any size. On or \
     off; off (default) uses the font glyph and is identical to before.";

/// Human-readable help for the symbol / Nerd-font fallback enable switch
/// (RV6), shown in the in-app settings panel.
pub const SYMBOL_FALLBACK_DESC: &str = "Symbol fallback: enables a secondary symbol/Nerd-font face for private-use \
     prompt icons when the main font lacks a glyph. Off by default and \
     identical to the plain missing-glyph path. Environment override wins.";

/// Human-readable help for the optional explicit symbol / Nerd-font path
/// (RV6), shown in the in-app settings panel.
pub const SYMBOL_FONT_DESC: &str = "Symbol font: optional path to a .ttf/.otf symbol or Nerd Font used when \
     symbol_fallback is on. Empty uses OdyTTY's automatic symbol-font search. \
     ODYTTY_SYMBOL_FONT wins over this setting.";
