// SPDX-License-Identifier: GPL-3.0-only
/// Human-readable help for the stem-darken knob, destined for the in-app
/// settings panel (UX2). Establishes the convention that every new knob ships
/// with a concise description, its accepted values, and its default.
pub const STEM_DARKEN_DESC: &str = "Stem darkening: boosts glyph coverage so light-on-dark text holds weight at \
     small sizes. Accepts 0.0–1.0; 0.0 is off (identical to no boost), 1.0 is \
     strongest. Default 0.5.";

/// Human-readable help for the minimum-contrast knob, shown in the in-app
/// settings panel (UX2). Follows the every-knob-carries-a-description convention.
pub const MIN_CONTRAST_DESC: &str = "Minimum contrast: lifts foreground text so its WCAG contrast against the \
     background meets at least this ratio, keeping low-contrast apps legible. \
     Accepts 1.0–21.0; 1.0 is off (no change), 4.5 is the WCAG AA body-text \
     threshold, 7.0 is AAA. Hue is preserved. Default 13.0.";

/// Human-readable help for the focus-dimming knob (ID2), shown in the in-app
/// settings panel. Follows the every-knob-carries-a-description convention.
pub const FOCUS_DIM_DESC: &str = "Focus dimming: dims the whole window (text and background) while it is \
     unfocused so it recedes visually, in OKLab so hue is preserved. Accepts \
     0.0–1.0; 0.0 is off (no change, focused and unfocused look identical), \
     0.15–0.30 is a subtle recede. The focused window is never dimmed. The \
     minimum-contrast floor still applies, so text stays legible. Default 0.0.";

pub const RENDER_QUALITY_DESC: &str = "Renderer profile: balanced is the default quality path — all enabled effects \
     (bloom, CRT scanlines, background treatment, focus dimming, stem darkening, \
     and the minimum-contrast floor) are honored. plain is the hard fast path: \
     it forces all of those off even when their individual settings are enabled, \
     giving raw speed with no post-process cost. high is reserved for future \
     higher-cost quality paths and currently behaves like balanced.";

/// Human-readable help for the ID3/U5 background-treatment knob, shown in the
/// in-app settings panel.
pub const BACKGROUND_TREATMENT_DESC: &str = "Background treatment: subtly darkens the cell background by position so the \
     window has depth. off (default) draws the background unchanged and is \
     pixel-identical to before. gradient darkens toward the bottom; vignette \
     darkens toward the edges and corners. image draws a wallpaper behind the grid \
     (see background_image + cell_bg_opacity). The minimum-contrast floor is \
     applied to the treated background, so text stays legible by construction. \
     Small extra per-frame cost only while a rebuild runs; off when the renderer \
     profile is plain.";

pub const BACKGROUND_IMAGE_DESC: &str = "Background image: path to a PNG, JPEG, or WebP drawn behind the terminal grid when the \
     background treatment is set to image. Empty (default) means no image. Pair \
     with cell_bg_opacity below 1.0 to let it show through behind text; a \
     readability scrim is computed automatically so text stays legible at any \
     opacity. A missing or undecodable file is ignored with a warning.";

pub const BACKGROUND_BLUR_RADIUS_DESC: &str = "Background blur: pixel radius of a one-time blur applied to the background \
     image at load. 0 (default) keeps the image sharp. Larger values soften it \
     so text stays readable over busy images. Computed once on the CPU; no \
     per-frame cost.";

pub const BACKGROUND_IMAGE_SCRIM_DESC: &str = "Wallpaper readability: auto or explicit 0.0-1.0 strength of the readability \
     overlay blended over the wallpaper. auto (default) computes the minimum \
     overlay that keeps text legible. Lower values keep the image clearer; \
     higher values make text safer over busy images.";

pub const CELL_BG_OPACITY_DESC: &str = "Wallpaper visibility: how much of the wallpaper shows through terminal cell \
     backgrounds. 0.0 (default) hides the wallpaper behind cells and preserves \
     the original solid terminal look. Higher values reveal more of the image; \
     the config/env key stores the inverse as cell background opacity.";

pub const WINDOW_PADDING_DESC: &str = "Window padding: logical pixels of inset between the window edge and the \
     terminal grid. Accepts 0.0-64.0; 0.0 restores the historical edge-to-edge \
     layout exactly. Default 4.0.";

pub const SCROLL_DRAG_SPEED_DESC: &str = "Drag autoscroll speed: when you drag a selection past the top or bottom \
     edge, ramp accelerates the scroll the further past the edge you drag \
     (capped so it never runs away); legacy holds a steady one row per step. \
     Affects local selection only. Default ramp.";

pub const BLOOM_DESC: &str = "Bloom: HDR phosphor glow over bright cells. On in the Odyssey ambient \
     default and pixel-identical to the plain renderer when off. Requires a GPU with filterable \
     Rgba16Float render targets; unsupported adapters silently use the plain path.";
pub const BLOOM_THRESHOLD_DESC: &str = "Bloom threshold: luminance level above which text begins to glow. The default is \
     0.75 for the Odyssey ambient baseline; lower values glow more text, higher values reserve \
     bloom for brighter elements.";
pub const BLOOM_INTENSITY_DESC: &str = "Bloom intensity: additive glow strength. Accepts 0.0–1.0; 0.0 emits no \
     glow, 0.8 is the ambient default, and the cap keeps bloom bounded.";
pub const BLOOM_RADIUS_DESC: &str = "Bloom radius: blur spread in half-resolution pixels. Accepts 0.5–8.0; \
     8.0 is the ambient default wide phosphor radius.";

pub const RETRO_DESC: &str = "Retro preset: one-switch stronger phosphor look. When on, bloom and CRT use a tuned high-visibility profile without overwriting the individual knobs. The plain renderer profile still bypasses it.";
pub const CRT_DESC: &str = "CRT profile: post-process scanlines plus vignette. On in the Odyssey ambient default and pixel-identical to the plain renderer when off. Requires the same adapter support as bloom; unsupported adapters silently use the plain path.";
pub const CRT_SCANLINE_INTENSITY_DESC: &str = "CRT scanline intensity: bounded multiplicative dimming for the dark part of each scanline. Accepts 0.0–0.35; the shader keeps a brightness floor so scanlines stay readable.";
pub const CRT_SCANLINE_PERIOD_DESC: &str = "CRT scanline spacing: vertical distance between the dark scanline bands in the CRT/retro \
     post-process, measured in physical pixels. Smaller values pack bands closer together; \
     larger values spread them out for a coarser look. Accepts 2.0–12.0; 7.0 is the \
     ambient default. Only takes effect when the CRT profile is on.";
pub const CRT_VIGNETTE_STRENGTH_DESC: &str = "CRT vignette strength: bounded edge dimming. Accepts 0.0–0.45; the shader enforces a brightness floor so corners recede without erasing lit cells.";
pub const CRT_CURVATURE_DESC: &str = "CRT curvature: subtle barrel-distortion screen curvature. Accepts 0.0–0.12; 0.0 (default) is flat and pixel-identical to the plain renderer, higher values gently bow the screen with UV clamping so borders stay free of black seams. Only takes effect when the CRT profile is on; the retro preset overrides it to a light curve.";

/// Human-readable help for the geometric box-drawing knob (RV2), shown in the
/// in-app settings panel. Follows the every-knob-carries-a-description convention.
pub const GEOMETRIC_BOXDRAW_DESC: &str = "Geometric box-drawing: renders line, block and Powerline glyphs from \
     cell-aligned geometry instead of the font, so TUI borders, progress bars \
     and powerline prompts are pixel-perfect and seamless at any size. On or \
     off; off (default) uses the font glyph and is identical to before.";

/// Human-readable help for the symbol / Nerd-font fallback enable switch
/// (RV6), shown in the in-app settings panel.
pub const SYMBOL_FALLBACK_DESC: &str = "Symbol fallback: enables a secondary symbol/Nerd-font face for private-use \
     prompt icons when the main font lacks a glyph. On by default for common \
     shell prompts; switch off to force the plain missing-glyph path. Environment override wins.";

/// Human-readable help for the optional explicit symbol / Nerd-font path
/// shown in the in-app settings panel.
pub const SYMBOL_FONT_DESC: &str = "Symbol font file: optional .ttf/.otf path used when symbol fallback is on; \
     routes Private-Use-Area icon codepoints (Nerd Font prompt icons) to this \
     face. Empty or auto uses OdyTTY's host search, then the bundled symbols face. \
     ODYTTY_SYMBOL_FONT wins.";
